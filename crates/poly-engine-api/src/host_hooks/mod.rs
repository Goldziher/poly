//! Cache-path hooks that run in the host, never in an engine component.
//!
//! The stat fast path and every cache lookup need a handful of per-run and per-file inputs that
//! live outside the file's bytes: the oxlint / oxfmt config governing a path, a Python file's
//! package root and target version, the module tree isort probes. They must be computable without
//! loading the engine that consumes them, otherwise a warm run would have to open a component
//! dylib just to learn that nothing changed. So they live here, in a crate with no engine
//! dependencies, and an engine states which of them it uses by returning a [`HostHook`] from
//! [`Engine::host_hook`](crate::engine::Engine::host_hook).
//!
//! In-process engines and the future component proxy call the same functions below, so both
//! produce byte-identical cache keys: the [`Engine`](crate::engine::Engine) trait's default
//! `run_context`, `file_context`, `lint_input_salt`, `format_input_salt` and `format_config`
//! delegate to them.
//!
//! The one-per-process warning guard ([`warn_once`]) lives here for the same reason: it fires from
//! host-side config translation, so a single static in this crate keeps a warning from repeating
//! once the crate is linked into more than one library.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::config::EngineConfig;
use crate::engine::SourceFile;

pub mod astgrep_rules;
pub mod oxfmt_native;
pub mod oxfmt_scope;
pub mod oxlintrc;
pub mod path_ignore;
pub mod ruff_context;
pub mod ruff_target;
pub mod typos_options;

/// Which host-side cache-path inputs an engine's results depend on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostHook {
    /// The options table and the file's bytes say everything.
    #[default]
    None,
    /// Formatting follows the path-scoped `.oxfmtrc.json[c]` (nested configs and `overrides`):
    /// taplo, rumdl, yaml and malva.
    OxfmtScope,
    /// [`HostHook::OxfmtScope`] for formatting plus the nearest `.oxlintrc.json[c]` chain for
    /// linting: the oxc engine.
    Oxc,
    /// Ruff's lint depends on the package root, the module tree under the source roots and the
    /// target Python version the project declares.
    RuffContext,
}

/// Inputs shared by every file of a run (see [`Engine::run_context`](crate::engine::Engine::run_context)).
#[must_use]
pub fn run_context(hook: HostHook, cfg: &EngineConfig) -> String {
    match hook {
        HostHook::None | HostHook::OxfmtScope | HostHook::Oxc => String::new(),
        HostHook::RuffContext => {
            let src_roots: Vec<std::path::PathBuf> = cfg
                .options
                .get("src")
                .and_then(toml::Value::as_array)
                .map(|roots| roots.iter().filter_map(toml::Value::as_str).map(Into::into).collect())
                .unwrap_or_default();
            ruff_context::forget_package_roots();
            ruff_target::forget_targets();
            ruff_context::module_tree(&src_roots)
        }
    }
}

/// Inputs specific to one file (see [`Engine::file_context`](crate::engine::Engine::file_context)).
#[must_use]
pub fn file_context(hook: HostHook, path: &Path, cfg: &EngineConfig) -> Option<String> {
    match hook {
        HostHook::None => None,
        HostHook::OxfmtScope => format_input_salt(hook, path, cfg),
        HostHook::Oxc => match (lint_input_salt(hook, path, cfg), format_input_salt(hook, path, cfg)) {
            (None, None) => None,
            (lint, format) => Some(format!("{}|{}", lint.unwrap_or_default(), format.unwrap_or_default())),
        },
        HostHook::RuffContext => {
            let target =
                ruff_target::project_target(path, cfg).map_or_else(|| "-".to_owned(), ruff_target::format_version);
            Some(format!("{}\u{0}target={target}", ruff_context::run_package_root(path)))
        }
    }
}

/// Extra input the *lint* result depends on (see
/// [`Engine::lint_input_salt`](crate::engine::Engine::lint_input_salt)).
#[must_use]
pub fn lint_input_salt(hook: HostHook, path: &Path, cfg: &EngineConfig) -> Option<String> {
    match hook {
        HostHook::Oxc => oxlintrc::governing_for(cfg, path).map(|governing| governing.digest.clone()),
        HostHook::None | HostHook::OxfmtScope | HostHook::RuffContext => None,
    }
}

/// Extra input the *format* result depends on (see
/// [`Engine::format_input_salt`](crate::engine::Engine::format_input_salt)).
#[must_use]
pub fn format_input_salt(hook: HostHook, path: &Path, cfg: &EngineConfig) -> Option<String> {
    match hook {
        HostHook::OxfmtScope | HostHook::Oxc => oxfmt_scope::input_salt(cfg, path),
        HostHook::None | HostHook::RuffContext => None,
    }
}

/// The config to format `src` with (see [`Engine::format_config`](crate::engine::Engine::format_config));
/// `engine` is the engine's [`name`](crate::engine::Engine::name).
#[must_use]
pub fn format_config<'a>(
    hook: HostHook,
    engine: &str,
    src: &SourceFile,
    cfg: &'a EngineConfig,
) -> Cow<'a, EngineConfig> {
    match hook {
        HostHook::OxfmtScope | HostHook::Oxc => oxfmt_scope::path_config(cfg, src, engine),
        HostHook::None | HostHook::RuffContext => Cow::Borrowed(cfg),
    }
}

/// Run `report` the first time `key` is seen in this process, and never again.
///
/// One static for the whole host: warnings raised while translating a native tool config (oxfmt
/// options poly cannot honor, an unreadable config) must not repeat per language, per engine or
/// per linked copy of this crate.
pub fn warn_once(key: String, report: impl FnOnce()) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key);
    if first {
        report();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;
    use crate::language::Language;

    fn cfg(options: toml::Table) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options,
        }
    }

    fn source(path: PathBuf) -> SourceFile {
        SourceFile {
            path,
            language: Language::TypeScript,
            content: "".into(),
        }
    }

    #[test]
    fn the_none_hook_adds_nothing_to_the_cache_key() {
        let cfg = cfg(toml::Table::new());
        assert_eq!(run_context(HostHook::None, &cfg), "");
        assert_eq!(file_context(HostHook::None, Path::new("a.ts"), &cfg), None);
        assert_eq!(lint_input_salt(HostHook::None, Path::new("a.ts"), &cfg), None);
        assert_eq!(format_input_salt(HostHook::None, Path::new("a.ts"), &cfg), None);
        let src = source("a.ts".into());
        assert!(matches!(
            format_config(HostHook::None, "oxc", &src, &cfg),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn the_oxc_hook_combines_the_lint_and_format_salts() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let lintrc = repo.path().join(".oxlintrc.json");
        let fmtrc = repo.path().join(".oxfmtrc.json");
        std::fs::write(&lintrc, r#"{"rules": {}}"#).unwrap();
        std::fs::write(&fmtrc, "{}").unwrap();
        let mut options = toml::Table::new();
        options.insert(
            oxlintrc::RC_KEY.into(),
            toml::Value::String(lintrc.display().to_string()),
        );
        options.insert(
            oxfmt_scope::RC_KEY.into(),
            toml::Value::String(fmtrc.display().to_string()),
        );
        let cfg = cfg(options);
        let path = repo.path().join("a.ts");

        let lint = lint_input_salt(HostHook::Oxc, &path, &cfg).expect("an oxlint config governs the file");
        let format = format_input_salt(HostHook::Oxc, &path, &cfg).expect("an oxfmt config governs the file");
        assert_eq!(
            file_context(HostHook::Oxc, &path, &cfg),
            Some(format!("{lint}|{format}"))
        );
        assert_eq!(file_context(HostHook::OxfmtScope, &path, &cfg), Some(format));
        assert_eq!(lint_input_salt(HostHook::OxfmtScope, &path, &cfg), None);
    }

    #[test]
    fn the_oxc_hook_without_configs_contributes_nothing() {
        let cfg = cfg(toml::Table::new());
        assert_eq!(file_context(HostHook::Oxc, Path::new("a.ts"), &cfg), None);
    }

    #[test]
    fn the_ruff_hook_keys_on_the_package_root_and_the_discovered_target() {
        let repo = tempfile::tempdir().unwrap();
        let pkg = repo.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), "").unwrap();
        std::fs::write(repo.path().join("ruff.toml"), "target-version = \"py310\"\n").unwrap();
        let cfg = cfg(toml::Table::new());
        ruff_context::forget_package_roots();
        ruff_target::forget_targets();

        let context = file_context(HostHook::RuffContext, &pkg.join("m.py"), &cfg).unwrap();
        assert_eq!(context, format!("{}\u{0}target=3.10", pkg.display()));
        assert_eq!(lint_input_salt(HostHook::RuffContext, &pkg.join("m.py"), &cfg), None);
    }

    #[test]
    fn the_ruff_run_context_follows_the_module_tree() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("a")).unwrap();
        std::fs::write(repo.path().join("a/x.py"), "").unwrap();
        let mut options = toml::Table::new();
        options.insert(
            "src".into(),
            toml::Value::Array(vec![toml::Value::String(repo.path().display().to_string())]),
        );
        let cfg = cfg(options);
        let before = run_context(HostHook::RuffContext, &cfg);
        std::fs::write(repo.path().join("a/y.py"), "").unwrap();
        assert_ne!(before, run_context(HostHook::RuffContext, &cfg));
        assert_eq!(run_context(HostHook::None, &cfg), "");
    }

    #[test]
    fn a_warning_fires_once_per_key() {
        let mut count = 0;
        for _ in 0..3 {
            warn_once("host-hooks-test-key".to_owned(), || count += 1);
        }
        assert_eq!(count, 1);
    }
}
