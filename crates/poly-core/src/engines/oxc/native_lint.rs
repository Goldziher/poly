//! Linting JS/TS with the project's own `.oxlintrc.json[c]` instead of poly's opinionated rule set.
//!
//! The config governing a file is the nearest one above it (see [`crate::host_hooks::oxlintrc`]); one
//! [`SourceLinter`] is built per config and reused. `[lint.<lang>.oxc]` `select` / `extend_select` /
//! `ignore` / `[rules.<id>]` levels are layered on top, like oxlint's own `-A` / `-D` / `-W` flags.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use oxc_linter::{AllowWarnDeny, ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintFilter, Message, Oxlintrc};

use super::run::SourceLinter;
use crate::config::EngineConfig;
use crate::engine::{Severity, SourceFile};
use crate::engines::rule_config::RuleSelection;
pub(super) use crate::host_hooks::oxlintrc::governing_for;

/// Report a config's `jsPlugins` once: they are JavaScript modules, which poly's in-process
/// linter has no runtime to execute, so the rules they define are not applied.
fn warn_about_js_plugins(file: &Path, config: &Oxlintrc) {
    let mut specifiers: Vec<&str> = config
        .external_plugins
        .iter()
        .flatten()
        .map(|entry| entry.specifier.as_str())
        .collect();
    specifiers.sort_unstable();
    if !specifiers.is_empty() {
        tracing::warn!(
            "oxlint jsPlugins in {} are not run: they are JavaScript modules and poly lints in-process \
             without a JS runtime, so the rules they define are skipped ({}). Run oxlint for those rules.",
            file.display(),
            specifiers.join(", ")
        );
    }
}

/// The service for `config`, built once per (config file, options table).
pub(super) fn service_for(cfg: &EngineConfig, config: &Path) -> anyhow::Result<Arc<SourceLinter>> {
    type Slot = Arc<OnceLock<Result<Arc<SourceLinter>, String>>>;
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
pub(super) fn run(service: &SourceLinter, src: &SourceFile) -> Vec<Message> {
    let absolute = std::path::absolute(&src.path).unwrap_or_else(|_| src.path.clone());
    service.run(&absolute, &src.content)
}

fn build(file: &Path, selection: &RuleSelection) -> anyhow::Result<SourceLinter> {
    let oxlintrc = Oxlintrc::from_file(file).map_err(|error| anyhow::anyhow!("{error}"))?;
    warn_about_js_plugins(file, &oxlintrc);
    let mut store = ExternalPluginStore::new(false);
    let config = ConfigStoreBuilder::from_oxlintrc(false, oxlintrc, None, &mut store, None)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?
        .with_filters(&user_filters(selection))
        .build(&mut store)
        .map_err(|error| anyhow::anyhow!("{}: {error}", file.display()))?;
    Ok(SourceLinter::new(
        super::lint::lint_options(),
        ConfigStore::new(config, Default::default(), store),
    ))
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
