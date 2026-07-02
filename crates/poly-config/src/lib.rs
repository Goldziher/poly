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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

mod cache;
mod commit;
mod defaults;
mod hooks;
mod tools;

pub use cache::{CacheConfig, HookCacheMode, ResultsCacheConfig, SccacheConfig};
pub use commit::{CleanupRule, CommitConfig, CommitRules, ExcludeRule, MessageRule};
pub use defaults::{GlobalDefaults, LineEnding};
pub use hooks::{
    BuiltinHook, BuiltinHooks, CargoHooks, DEFAULT_MAX_ADDED_FILE_KB, FileSafetyHooks, Guard, GuardCondition,
    GuardMatch, HooksConfig, Job, JobCache, ParseStageError, Patterns, Stage, StageConfig,
};
pub use tools::{ToolConfig, ToolsConfig};

/// Config file names in precedence order: `poly.toml` wins over `polylint.toml`
/// within the same directory.
pub const CONFIG_FILE_NAMES: [&str; 2] = ["poly.toml", "polylint.toml"];

/// Name of the optional local override file deep-merged over the primary config
/// when it sits in the same directory (issue #2193). Scalars and arrays in the
/// override replace the base; tables are merged recursively.
pub const LOCAL_OVERRIDE_NAME: &str = "poly.local.toml";

/// The fully parsed `poly.toml` (or back-compat `polylint.toml`).
///
/// `lint` and `fmt` are left as raw [`toml::Table`]s here; `polylint-core`
/// slices them per language and engine.
#[derive(Debug, Clone, Default)]
pub struct PolyConfig {
    /// `[defaults]` — opinionated global defaults.
    pub defaults: GlobalDefaults,
    /// `[discovery]` — file-walk tuning for direct `poly lint`/`poly fmt`/`poly cache`.
    pub discovery: DiscoveryConfig,
    /// `[lint.<lang>.<tool>]` tables.
    pub lint: toml::Table,
    /// `[fmt.<lang>.<tool>]` tables.
    pub fmt: toml::Table,
    /// `[commit]` — `poly commit` configuration.
    pub commit: CommitConfig,
    /// `[hooks]` — `poly hooks` configuration.
    pub hooks: HooksConfig,
    /// `[cache]` — result-cache and sccache configuration.
    pub cache: CacheConfig,
    /// `[tools.<name>]` — opted-in vendored catalog tools (ADR 0013).
    pub tools: ToolsConfig,
    /// `[per-file-ignores]` — map of gitignore-style path glob → rule codes to
    /// suppress for files matching that glob (lint-only). Codes are matched
    /// against the normalized `Diagnostic.code` (exact or prefix), so a single
    /// table covers every backend (e.g. ruff `F401`, mago `too-many-methods`).
    pub per_file_ignores: BTreeMap<String, Vec<String>>,
}

/// `[discovery]` — tunes the file walk that direct `poly lint` / `poly fmt` /
/// `poly cache` runs (the CI / GitHub Action path).
///
/// The hooks path already excludes per-builtin; this gives the direct-CLI path
/// the same reach. Globs are gitignore-style and compose with `.gitignore` and
/// the built-in vendored/generated prune set — they never override an explicitly
/// passed path argument.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    /// Gitignore-style globs excluded from discovery. Accepts a single string or
    /// an array (`exclude = "test_apps/**"` or `exclude = ["a/**", "b/**"]`),
    /// matching the `files`/`exclude` shape used throughout `[hooks]`/`[tools]`.
    pub exclude: Patterns,
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
    ///
    /// If a [`LOCAL_OVERRIDE_NAME`] file sits next to `path`, it is deep-merged
    /// over the primary config before deserialization. The merged `[hooks]`
    /// table is then validated (see [`HooksConfig::validate`]).
    pub fn load_file(path: &Path) -> anyhow::Result<PolyConfig> {
        let text = std::fs::read_to_string(path)?;
        let mut table: toml::Table = toml::from_str(&text)?;

        if let Some(parent) = path.parent() {
            let override_path = parent.join(LOCAL_OVERRIDE_NAME);
            if override_path.is_file() {
                let override_text = std::fs::read_to_string(&override_path)?;
                let override_table: toml::Table = toml::from_str(&override_text)?;
                merge_tables(&mut table, override_table);
            }
        }

        let raw: RawPolyConfig = table.try_into()?;
        let config: PolyConfig = raw.into();
        config
            .hooks
            .validate()
            .map_err(|message| anyhow::anyhow!("invalid [hooks] config: {message}"))?;
        config
            .tools
            .validate()
            .map_err(|message| anyhow::anyhow!("invalid [tools] config: {message}"))?;
        Ok(config)
    }
}

/// Recursively deep-merge `override_table` over `base`. Two tables at the same
/// key are merged key-by-key; any other value (scalar or array) in the override
/// replaces the base value.
fn merge_tables(base: &mut toml::Table, override_table: toml::Table) {
    for (key, override_value) in override_table {
        match (base.get_mut(&key), override_value) {
            (Some(toml::Value::Table(base_child)), toml::Value::Table(override_child)) => {
                merge_tables(base_child, override_child);
            }
            (_, override_value) => {
                base.insert(key, override_value);
            }
        }
    }
}

/// Find the nearest config file, walking upward from `start`. Within each
/// directory `poly.toml` is preferred over `polylint.toml`.
pub fn find_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_file() { start.parent()? } else { start };
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
    discovery: DiscoveryConfig,
    lint: toml::Table,
    fmt: toml::Table,
    commit: CommitConfig,
    hooks: HooksConfig,
    cache: CacheConfig,
    tools: ToolsConfig,
    #[serde(rename = "per-file-ignores")]
    per_file_ignores: BTreeMap<String, Vec<String>>,
}

impl From<RawPolyConfig> for PolyConfig {
    fn from(raw: RawPolyConfig) -> Self {
        PolyConfig {
            defaults: raw.defaults.into(),
            discovery: raw.discovery,
            lint: raw.lint,
            fmt: raw.fmt,
            commit: raw.commit,
            hooks: raw.hooks,
            cache: raw.cache,
            tools: raw.tools,
            per_file_ignores: raw.per_file_ignores,
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
        assert!(config.hooks.stage_configs.is_empty());
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
    fn parses_discovery_exclude() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[discovery]
exclude = ["test_apps/**", "artifacts/**"]
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(
            config.discovery.exclude.as_slice(),
            &["test_apps/**".to_string(), "artifacts/**".to_string()],
        );
    }

    #[test]
    fn parses_per_file_ignores() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            "[per-file-ignores]\n\"tests/**\" = [\"F401\", \"too-many-methods\"]\n\"*.gen.py\" = [\"E501\"]\n",
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(
            config.per_file_ignores.get("tests/**"),
            Some(&vec!["F401".to_string(), "too-many-methods".to_string()]),
        );
        assert_eq!(config.per_file_ignores.get("*.gen.py"), Some(&vec!["E501".to_string()]),);
    }

    #[test]
    fn discovery_exclude_accepts_a_single_string() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(&path, "[discovery]\nexclude = \"test_apps/**\"\n").unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(config.discovery.exclude.as_slice(), &["test_apps/**".to_string()]);
    }

    #[test]
    fn absent_discovery_table_yields_no_excludes() {
        let dir = tempdir().unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert!(config.discovery.exclude.is_empty());
    }

    #[test]
    fn poly_toml_wins_over_polylint_toml_in_same_dir() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("poly.toml"), "[defaults]\nline_length = 80\n").unwrap();
        fs::write(dir.path().join("polylint.toml"), "[defaults]\nline_length = 200\n").unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert_eq!(config.defaults.line_length, 80, "poly.toml should win");
    }

    #[test]
    fn falls_back_to_polylint_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("polylint.toml"), "[defaults]\nline_length = 77\n").unwrap();
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
    fn absent_cache_table_yields_defaults() {
        let dir = tempdir().unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert!(config.cache.enabled, "cache.enabled must default to true");
        assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
        assert!(!config.cache.sccache.enabled, "sccache.enabled must default to false");
        assert!(config.cache.dir.is_none());
    }

    #[test]
    fn parses_cache_table_full() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[cache]
enabled = true

[cache.results]
hooks = "safe"

[cache.sccache]
enabled = true
bin = "/usr/bin/sccache"
dir = "/tmp/sccache"
max_size = "5G"
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert!(config.cache.enabled);
        assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
        assert!(config.cache.sccache.enabled);
        assert_eq!(config.cache.sccache.bin.as_deref(), Some("/usr/bin/sccache"));
        assert_eq!(config.cache.sccache.dir.as_deref(), Some("/tmp/sccache"));
        assert_eq!(config.cache.sccache.max_size.as_deref(), Some("5G"));
    }

    #[test]
    fn parses_cache_mode_off_and_aggressive() {
        let dir = tempdir().unwrap();
        let off_path = dir.path().join("off.toml");
        fs::write(&off_path, "[cache.results]\nhooks = \"off\"\n").unwrap();
        let config_off = PolyConfig::load_file(&off_path).expect("load off");
        assert_eq!(config_off.cache.results.hooks, crate::HookCacheMode::Off);

        let agg_path = dir.path().join("agg.toml");
        fs::write(&agg_path, "[cache.results]\nhooks = \"aggressive\"\n").unwrap();
        let config_agg = PolyConfig::load_file(&agg_path).expect("load aggressive");
        assert_eq!(config_agg.cache.results.hooks, crate::HookCacheMode::Aggressive);
    }

    #[test]
    fn parses_cache_disabled_with_dir_override() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(&path, "[cache]\nenabled = false\ndir = \"/custom/cache\"\n").unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert!(!config.cache.enabled);
        assert_eq!(config.cache.dir.as_deref(), Some("/custom/cache"));
    }

    #[test]
    fn parses_hooks_builtin_and_inline_stages() {
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

[hooks.pre-commit]
parallel = true
[[hooks.pre-commit.jobs]]
run = "cargo fmt --check"
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
        // inline stage
        let pre_commit = &config.hooks.stage_configs[&Stage::PreCommit];
        assert!(pre_commit.parallel);
        assert_eq!(pre_commit.jobs.len(), 1);
        assert_eq!(pre_commit.jobs[0].run.as_deref(), Some("cargo fmt --check"));
    }

    #[test]
    fn imported_repos_are_rejected_at_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[[hooks.repo]]
repo = "https://github.com/example/hooks"
"#,
        )
        .unwrap();
        let error = PolyConfig::load_file(&path).unwrap_err().to_string();
        assert!(error.contains("no longer supported"), "{error}");
    }

    #[test]
    fn invalid_hooks_job_fails_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
run = "x"
script = "y.sh"
"#,
        )
        .unwrap();
        let error = PolyConfig::load_file(&path).unwrap_err().to_string();
        assert!(error.contains("invalid [hooks] config"), "{error}");
    }

    #[test]
    fn local_override_deep_merges_nested_value() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("poly.toml"),
            r#"
[defaults]
line_length = 100
[cache.results]
hooks = "safe"
"#,
        )
        .unwrap();
        fs::write(
            dir.path().join(LOCAL_OVERRIDE_NAME),
            r#"
[defaults]
line_length = 80
"#,
        )
        .unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        // Overridden nested scalar takes the local value...
        assert_eq!(config.defaults.line_length, 80);
        // ...while untouched nested tables are preserved from the base.
        assert_eq!(config.cache.results.hooks, crate::HookCacheMode::Safe);
    }

    #[test]
    fn parses_tools_table_from_poly_toml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(
            &path,
            r#"
[tools.shfmt]
enabled = true
args = ["-i", "2"]
stages = ["pre-commit"]

[tools.clang-format]
enabled = true
"#,
        )
        .unwrap();
        let config = PolyConfig::load_file(&path).expect("load");
        assert_eq!(config.tools.len(), 2);
        let shfmt = config.tools.get("shfmt").expect("shfmt present");
        assert!(shfmt.enabled);
        assert_eq!(shfmt.args.as_deref(), Some(&["-i".to_string(), "2".to_string()][..]));
        assert_eq!(shfmt.stages, vec![Stage::PreCommit]);
        assert!(config.tools.get("clang-format").unwrap().enabled);
    }

    #[test]
    fn unknown_tool_name_fails_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("poly.toml");
        fs::write(&path, "[tools.not-a-real-tool]\nenabled = true\n").unwrap();
        let error = PolyConfig::load_file(&path).unwrap_err().to_string();
        assert!(error.contains("invalid [tools] config"), "{error}");
        assert!(error.contains("not-a-real-tool"), "{error}");
    }

    #[test]
    fn absent_tools_table_yields_empty() {
        let dir = tempdir().unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert!(config.tools.is_empty());
    }

    #[test]
    fn absent_local_override_is_a_no_op() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("poly.toml"), "[defaults]\nline_length = 99\n").unwrap();
        let config = PolyConfig::load(dir.path()).expect("load");
        assert_eq!(config.defaults.line_length, 99);
    }
}
