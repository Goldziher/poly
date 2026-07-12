//! Unified `poly.toml` configuration schema shared by every `poly` surface:
//! `poly lint` / `poly fmt` (the `[defaults]`, `[lint.*]`, `[fmt.*]` tables),
//! `poly hooks` (`[hooks]`), and `poly commit` (`[commit]`).
//!
//! This crate owns only the **on-disk schema and its parsing** — it has no
//! dependency on the engine layer, so all four surfaces can share one parsed
//! [`PolyConfig`] without coupling. Language-aware slicing (turning the `[lint]`
//! / `[fmt]` tables into a per-engine config) lives in `poly-core`.
//!
//! The config file is `poly.toml`. Discovery walks upward from a start directory
//! until it finds one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

mod cache;
mod commit;
mod defaults;
mod hook_sources;
mod hooks;
mod tools;
mod typos_native;

pub use cache::{CacheConfig, HookCacheMode, ResultsCacheConfig, SccacheConfig};
pub use commit::{CleanupRule, CommitConfig, CommitRules, ExcludeRule, MessageRule};
pub use defaults::{GlobalDefaults, LineEnding};
pub use hook_sources::{
    HookInstallChannel, HookMachinePreferences, HookSource, HookSourceConfig, MissingToolchainPolicy,
    load_hook_source_config,
};
pub use hooks::{
    BuiltinHook, BuiltinHooks, CargoHooks, DEFAULT_MAX_ADDED_FILE_KB, FileSafetyHooks, Guard, GuardCondition,
    GuardMatch, HooksConfig, Job, JobCache, ParseStageError, Patterns, Stage, StageConfig,
};
pub use tools::{ToolConfig, ToolsConfig};
pub use typos_native::TyposNative;
use typos_native::resolve_typos_native;

/// The config file name poly discovers. A single-element list so the discovery
/// loops that iterate it stay unchanged if more names are ever added.
pub const CONFIG_FILE_NAMES: [&str; 1] = ["poly.toml"];

/// Name of the optional local override file deep-merged over the primary config
/// when it sits in the same directory (issue #2193). Scalars and arrays in the
/// override replace the base; tables are merged recursively.
pub const LOCAL_OVERRIDE_NAME: &str = "poly.local.toml";

/// The fully parsed `poly.toml`.
///
/// `lint` and `fmt` are left as raw [`toml::Table`]s here; `poly-core`
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
    /// Resolved native `_typos.toml` / `.typos.toml` content, if present near the
    /// config root. Combined with `[lint.typos]` in `poly-core`.
    pub typos_native: TyposNative,
    /// `[workspace]` — nested-config cascade boundary marker (ADR 0018).
    pub workspace: WorkspaceConfig,
    /// `[rules]` — custom ast-grep YAML rule directories.
    pub rules: RulesConfig,
}

/// `[rules]` — user-defined ast-grep YAML custom-rule directories.
///
/// Directories listed here are scanned for `*.yml` / `*.yaml` rule files on
/// every lint run. Paths are interpreted relative to the config file's directory.
/// The default is `[".poly/rules"]`; set to an empty array `dirs = []` to
/// disable custom rules entirely.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Directories (relative to the config file) containing custom ast-grep
    /// YAML rule files. Defaults to `[".poly/rules"]`.
    pub dirs: Vec<String>,
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            dirs: vec![".poly/rules".to_string()],
        }
    }
}

impl RulesConfig {
    /// Resolve every relative entry in `dirs` against `base` (the config file's
    /// directory), leaving absolute paths untouched.
    ///
    /// Rule directories are declared relative to the `poly.toml` that names them,
    /// so `poly lint` and `poly rules test` find the same rules regardless of the
    /// process working directory. Called once at load time with the config root.
    fn resolve_relative_to(&mut self, base: &Path) {
        for dir in &mut self.dirs {
            let path = Path::new(dir.as_str());
            if path.is_relative() {
                *dir = base.join(path).to_string_lossy().into_owned();
            }
        }
    }
}

/// `[workspace]` — marks a config as the cascade boundary for hierarchical
/// resolution (ADR 0018).
///
/// In a monorepo, `poly` resolves the config for a file by deep-merging the
/// chain of `poly.toml` files from the nearest one up to the workspace root.
/// Setting `root = true` stops that upward walk at this config, so a project
/// never inherits configuration from a `poly.toml` above its own root (e.g. one
/// in `$HOME`). A directory containing `.git` is treated as an implicit boundary
/// even without this marker.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    /// When `true`, upward cascade resolution stops here — this config is the
    /// base of the merge chain.
    pub root: bool,
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
            None => {
                let mut config = PolyConfig::default();
                let dir = if start.is_file() {
                    start.parent().unwrap_or(start)
                } else {
                    start
                };
                config.rules.resolve_relative_to(dir);
                config.typos_native = resolve_typos_native(dir);
                Ok(config)
            }
        }
    }

    /// Load config from an explicit file path.
    ///
    /// If a [`LOCAL_OVERRIDE_NAME`] file sits next to `path`, it is deep-merged
    /// over the primary config before deserialization. The merged `[hooks]`
    /// table is then validated (see [`HooksConfig::validate`]).
    pub fn load_file(path: &Path) -> anyhow::Result<PolyConfig> {
        let table = read_config_table(path)?;
        let typos_dir = path.parent().unwrap_or(path);
        finalize(table, typos_dir)
    }

    /// Resolve the effective config for `dir` by cascading the ancestor chain of
    /// config files — the workspace root as the base, the nearest config as the
    /// final override — deep-merged via `merge_tables` (ADR 0018). Each config
    /// in the chain first absorbs its sibling [`LOCAL_OVERRIDE_NAME`].
    ///
    /// The upward walk stops at (and includes) the first config marked
    /// `[workspace] root = true`, at a directory containing `.git`, or at the
    /// filesystem root. Returns the default config (with the nearest native
    /// typos config) when no config file is found — identical to [`load`] in the
    /// single-config case, so a repo with exactly one root `poly.toml` and no
    /// nested configs resolves exactly as before.
    ///
    /// [`load`]: PolyConfig::load
    pub fn resolve_for_dir(dir: &Path) -> anyhow::Result<PolyConfig> {
        let mut chain: Vec<(PathBuf, toml::Table)> = Vec::new();
        let mut current = Some(dir.to_path_buf());
        while let Some(d) = current {
            if let Some(path) = config_file_in(&d) {
                let mut table = read_config_table(&path)?;
                resolve_rules_dirs_in_table(&mut table, &d);
                let is_root = table_marks_workspace_root(&table);
                chain.push((d.clone(), table));
                if is_root {
                    break;
                }
            }
            if d.join(".git").exists() {
                break;
            }
            current = d.parent().map(Path::to_path_buf);
        }

        if chain.is_empty() {
            let mut config = PolyConfig {
                typos_native: resolve_typos_native(dir),
                ..PolyConfig::default()
            };
            config.rules.resolve_relative_to(dir);
            return Ok(config);
        }

        let mut iter = chain.into_iter().rev();
        let (mut nearest_dir, mut merged) = iter.next().expect("chain is non-empty");
        for (d, table) in iter {
            merge_tables(&mut merged, table);
            nearest_dir = d;
        }

        finalize(merged, &nearest_dir)
    }
}

/// Read a single config file into a [`toml::Table`], deep-merging its sibling
/// [`LOCAL_OVERRIDE_NAME`] over it when present.
fn read_config_table(path: &Path) -> anyhow::Result<toml::Table> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading config {}", path.display()))?;
    let mut table: toml::Table = toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
    if let Some(parent) = path.parent() {
        let override_path = parent.join(LOCAL_OVERRIDE_NAME);
        if override_path.is_file() {
            let override_text = std::fs::read_to_string(&override_path)
                .with_context(|| format!("reading config {}", override_path.display()))?;
            let override_table: toml::Table = toml::from_str(&override_text)
                .with_context(|| format!("parsing config {}", override_path.display()))?;
            merge_tables(&mut table, override_table);
        }
    }
    Ok(table)
}

/// Resolve relative `[rules] dirs` entries in a raw config table against `dir`
/// (the directory of the config file that declared them), leaving absolute
/// paths untouched. Applied during the cascade walk so each config's rule dirs
/// anchor at its own directory before tables are merged.
fn resolve_rules_dirs_in_table(table: &mut toml::Table, dir: &Path) {
    let Some(dirs) = table
        .get_mut("rules")
        .and_then(|v| v.as_table_mut())
        .and_then(|t| t.get_mut("dirs"))
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };
    for entry in dirs.iter_mut() {
        if let Some(relative) = entry.as_str().map(Path::new).filter(|p| p.is_relative()) {
            *entry = toml::Value::String(dir.join(relative).to_string_lossy().into_owned());
        }
    }
}

/// Deserialize a (possibly cascade-merged) config table into a validated
/// [`PolyConfig`], populating `typos_native` by searching upward from
/// `typos_dir`.
fn finalize(table: toml::Table, typos_dir: &Path) -> anyhow::Result<PolyConfig> {
    let raw: RawPolyConfig = table.try_into()?;
    let mut config: PolyConfig = raw.into();
    config.rules.resolve_relative_to(typos_dir);
    config.typos_native = resolve_typos_native(typos_dir);
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

/// Return the `poly.toml` in `dir`, if present (a single directory, no upward walk).
fn config_file_in(dir: &Path) -> Option<PathBuf> {
    for name in CONFIG_FILE_NAMES {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Whether a raw config table declares `[workspace] root = true`.
fn table_marks_workspace_root(table: &toml::Table) -> bool {
    table
        .get("workspace")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("root"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
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

/// Find the nearest `poly.toml`, walking upward from `start`.
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
    workspace: WorkspaceConfig,
    rules: RulesConfig,
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
            typos_native: TyposNative::default(),
            workspace: raw.workspace,
            rules: raw.rules,
        }
    }
}

#[cfg(test)]
mod tests;
