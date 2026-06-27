//! Unified `poly.toml` configuration schema shared by every `poly` surface:
//! `poly lint` / `poly fmt` (the `[defaults]`, `[lint.*]`, `[fmt.*]` tables),
//! `poly hooks` (`[hooks]`), and `poly commit` (`[commit]`).
//!
//! This crate owns only the **on-disk schema and its parsing** — it has no
//! dependency on the engine layer, so all four surfaces can share one parsed
//! [`PolyConfig`] without coupling. Language-aware slicing (turning the `[lint]`
//! / `[fmt]` tables into a per-engine config) lives in `polylint-core`.
//!
//! The canonical file is `poly.toml`; `polylint.toml` is read as a back-compat
//! fallback. Discovery walks upward from a start directory and, within each
//! directory, prefers `poly.toml` over `polylint.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

mod commit;
mod defaults;
mod hooks;

pub use commit::{CleanupRule, CommitConfig, CommitRules, ExcludeRule, MessageRule};
pub use defaults::{GlobalDefaults, LineEnding};
pub use hooks::{BuiltinHook, BuiltinHooks, HooksConfig, RepoHook, RepoHooks};

/// Config file names in precedence order: `poly.toml` wins over `polylint.toml`
/// within the same directory.
pub const CONFIG_FILE_NAMES: [&str; 2] = ["poly.toml", "polylint.toml"];

/// The fully parsed `poly.toml` (or back-compat `polylint.toml`).
///
/// `lint` and `fmt` are left as raw [`toml::Table`]s here; `polylint-core`
/// slices them per language and engine.
#[derive(Debug, Clone, Default)]
pub struct PolyConfig {
    /// `[defaults]` — opinionated global defaults.
    pub defaults: GlobalDefaults,
    /// `[lint.<lang>.<tool>]` tables.
    pub lint: toml::Table,
    /// `[fmt.<lang>.<tool>]` tables.
    pub fmt: toml::Table,
    /// `[commit]` — `poly commit` configuration.
    pub commit: CommitConfig,
    /// `[hooks]` — `poly hooks` configuration.
    pub hooks: HooksConfig,
}

impl PolyConfig {
    /// Load config by searching from `start` upward for a config file. Returns
    /// the default config when none is found.
    pub fn load(start: &Path) -> anyhow::Result<PolyConfig> {
        match find_config(start) {
            Some(path) => PolyConfig::load_file(&path),
            None => Ok(PolyConfig::default()),
        }
    }

    /// Load config from an explicit file path.
    pub fn load_file(path: &Path) -> anyhow::Result<PolyConfig> {
        let text = std::fs::read_to_string(path)?;
        let raw: RawPolyConfig = toml::from_str(&text)?;
        Ok(raw.into())
    }
}

/// Find the nearest config file, walking upward from `start`. Within each
/// directory `poly.toml` is preferred over `polylint.toml`.
pub fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() {
        start.parent()?
    } else {
        start
    };
    loop {
        for name in CONFIG_FILE_NAMES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        dir = dir.parent()?;
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct RawPolyConfig {
    defaults: defaults::RawDefaults,
    lint: toml::Table,
    fmt: toml::Table,
    commit: CommitConfig,
    hooks: HooksConfig,
}

impl From<RawPolyConfig> for PolyConfig {
    fn from(raw: RawPolyConfig) -> Self {
        PolyConfig {
            defaults: raw.defaults.into(),
            lint: raw.lint,
            fmt: raw.fmt,
            commit: raw.commit,
            hooks: raw.hooks,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn default_when_no_file_present() {
        let dir = tempdir().unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert_eq!(config.defaults.line_length, 120);
        assert!(config.lint.is_empty());
        assert!(config.hooks.repos.is_empty());
        assert!(!config.hooks.builtin.polylint.enabled);
    }

    #[test]
    fn parses_defaults_lint_and_fmt() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[defaults]
line_length = 100
line_ending = "crlf"

[lint.python.ruff]
select = ["E", "F"]

[fmt.javascript.oxc]
semicolons = true
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(config.defaults.line_length, 100);
        assert_eq!(config.defaults.line_ending, LineEnding::Crlf);
        assert!(config.lint.contains_key("python"));
        assert!(config.fmt.contains_key("javascript"));
    }

    #[test]
    fn poly_toml_wins_over_polylint_toml_in_same_dir() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("poly.toml"),
            "[defaults]\nline_length = 80\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("polylint.toml"),
            "[defaults]\nline_length = 200\n",
        )
        .unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert_eq!(config.defaults.line_length, 80, "poly.toml should win");
    }

    #[test]
    fn falls_back_to_polylint_toml() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("polylint.toml"),
            "[defaults]\nline_length = 77\n",
        )
        .unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert_eq!(config.defaults.line_length, 77);
    }

    #[test]
    fn parses_commit_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[commit]
preset = "conventional"
[commit.rules]
require_body = true
no_emojis = true
[[commit.rules.excludes]]
pattern = "^WIP"
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(config.commit.preset.as_deref(), Some("conventional"));
        assert_eq!(config.commit.rules.require_body, Some(true));
        assert_eq!(config.commit.rules.excludes.len(), 1);
        assert_eq!(config.commit.rules.excludes[0].pattern, "^WIP");
    }

    #[test]
    fn parses_hooks_builtin_toggle_and_table() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[hooks]
stages = ["pre-commit"]

[hooks.builtin]
polylint = true
polyfmt = { stages = ["pre-commit"] }
commit = { enabled = false }

[[hooks.repo]]
repo = "https://github.com/example/hooks"
rev = "v1.2.0"
hooks = [{ id = "some-hook", args = ["--fix"] }]
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(config.hooks.stages, vec!["pre-commit".to_string()]);
        // bare `true`
        assert!(config.hooks.builtin.polylint.enabled);
        assert!(config.hooks.builtin.polylint.stages.is_empty());
        // table without `enabled` → enabled
        assert!(config.hooks.builtin.polyfmt.enabled);
        assert_eq!(config.hooks.builtin.polyfmt.stages, vec!["pre-commit"]);
        // table with explicit `enabled = false`
        assert!(!config.hooks.builtin.commit.enabled);
        // foreign repo
        assert_eq!(config.hooks.repos.len(), 1);
        let repo = &config.hooks.repos[0];
        assert_eq!(repo.rev.as_deref(), Some("v1.2.0"));
        assert_eq!(repo.hooks[0].id, "some-hook");
        assert_eq!(repo.hooks[0].args, vec!["--fix"]);
    }
}
