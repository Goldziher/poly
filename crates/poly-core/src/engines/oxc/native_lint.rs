//! Linting JS/TS with the project's own `.oxlintrc.json[c]` instead of poly's opinionated rule set.
//!
//! The config governing a file is the nearest one above it (see [`crate::oxlint_native`]); one
//! [`LintService`] is built per config and reused. `[lint.<lang>.oxc]` `select` / `extend_select` /
//! `ignore` / `[rules.<id>]` levels are layered on top, like oxlint's own `-A` / `-D` / `-W` flags.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use oxc_linter::{
    AllowWarnDeny, ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintFilter, LintOptions, LintService,
    LintServiceOptions, Linter, Message, Oxlintrc,
};

use crate::config::EngineConfig;
use crate::engine::{Severity, SourceFile};
use crate::engines::rule_config::RuleSelection;
use crate::oxlint_native::{Governing, governing, warn_about_js_plugins};

/// The project's root oxlint config named by `cfg`, if any.
fn root_of(cfg: &EngineConfig) -> Option<&Path> {
    match cfg.options.get("oxlintrc") {
        Some(toml::Value::String(path)) => Some(Path::new(path)),
        Some(toml::Value::Boolean(_)) | None => None,
        Some(_) => None,
    }
}

/// The configs governing `src`, or `None` when the project has no oxlint config for this table.
pub(super) fn governing_for(cfg: &EngineConfig, path: &Path) -> Option<Arc<Governing>> {
    let root = root_of(cfg)?;
    Some(governing(root, path.parent().unwrap_or(Path::new(""))))
}

/// The service for `config`, built once per (config file, options table).
pub(super) fn service_for(cfg: &EngineConfig, config: &Path) -> anyhow::Result<Arc<LintService>> {
    type Slot = Arc<OnceLock<Result<Arc<LintService>, String>>>;
    static SERVICES: OnceLock<Mutex<HashMap<String, Slot>>> = OnceLock::new();
    let slot = {
        let mut services = SERVICES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            services
                .entry(format!("{}\0{}", config.display(), cfg.options))
                .or_default(),
        )
    };
    slot.get_or_init(|| {
        build(config, &RuleSelection::from_options(cfg))
            .map(Arc::new)
            .map_err(|error| format!("{error:#}"))
    })
    .clone()
    .map_err(|message| anyhow::anyhow!(message))
}

/// Run `service` over one source file with an absolute path, which is what override globs and
/// nested-config lookup are matched against.
pub(super) fn run(service: &LintService, src: &SourceFile) -> Vec<Message> {
    let absolute = std::path::absolute(&src.path).unwrap_or_else(|_| src.path.clone());
    let fs = super::lint::MemoryFileSystem {
        path: &absolute,
        content: &src.content,
    };
    service.run_source(&fs, vec![Arc::from(absolute.as_os_str())])
}

fn build(file: &Path, selection: &RuleSelection) -> anyhow::Result<LintService> {
    let oxlintrc = Oxlintrc::from_file(file).map_err(|error| anyhow::anyhow!("{error}"))?;
    warn_about_js_plugins(file, &oxlintrc);
    let mut store = ExternalPluginStore::new(false);
    let config = ConfigStoreBuilder::from_oxlintrc(false, oxlintrc, None, &mut store, None)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?
        .with_filters(&user_filters(selection))
        .build(&mut store)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?;
    let dir = file.parent().unwrap_or(Path::new("")).to_path_buf();
    let linter = Linter::new(
        LintOptions::default(),
        ConfigStore::new(config, Default::default(), store),
        None,
    );
    Ok(LintService::new(linter, LintServiceOptions::new(dir)))
}
/// The poly-side rule selection as oxlint filters, in the order oxlint applies its own flags.
fn user_filters(selection: &RuleSelection) -> Vec<LintFilter> {
    let mut filters = Vec::new();
    let mut push = |severity: AllowWarnDeny, name: &str| {
        if let Ok(filter) = LintFilter::new(severity, name.to_owned()) {
            filters.push(filter);
        }
    };
    for name in selection.select.iter().chain(&selection.extend_select) {
        push(AllowWarnDeny::Warn, name);
    }
    for name in &selection.ignore {
        push(AllowWarnDeny::Allow, name);
    }
    for (code, options) in &selection.rules {
        if let Some(level) = options.level {
            let severity = match level {
                Severity::Error => AllowWarnDeny::Deny,
                _ => AllowWarnDeny::Warn,
            };
            push(severity, code);
        }
    }
    filters
}
