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

use anyhow::{Context, bail};
use serde::Deserialize;

mod cache;
mod commit;
mod defaults;
pub mod extends;
mod hook_sources;
mod hooks;
mod merge;
mod tools;
mod typos_native;

use merge::{merge_layer, merge_tables};

pub use cache::{CacheConfig, HookCacheMode, ResultsCacheConfig, SccacheConfig};
pub use commit::{CleanupRule, CommitConfig, CommitRules, ExcludeRule, MessageRule};
pub use defaults::{GlobalDefaults, LineEnding};
pub use extends::{BaseConfigResolver, ExtendsSource, LocalPathResolver};
pub use hook_sources::{HookMachinePreferences, HookSource, load_hook_preferences};
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
    /// Apply `exclude` to explicitly named files too, not just to the walk.
    ///
    /// Off by default: naming a file on the command line should check it. The
    /// hook path turns this on, because a hook is handed staged paths rather
    /// than deliberate ones, and without it a repo's excludes are silently
    /// inert exactly where they matter most.
    pub force_exclude: bool,
}

impl PolyConfig {
    /// Load the effective config for `start`.
    ///
    /// When `start` sits inside a git repository, every `poly.toml` (each first
    /// absorbing its sibling [`LOCAL_OVERRIDE_NAME`]) from the repository root
    /// down to `start` is deep-merged via [`resolve_for_dir`] — the nearest
    /// config wins, inheriting from its ancestors. The cascade is bounded at the
    /// `.git` directory, so a stray `poly.toml` above the repository (e.g. one in
    /// `$HOME`) is never picked up. This is what makes a run from a subdirectory
    /// (`frontend/`) honor the repo-root config's settings and exclude globs.
    ///
    /// Outside a git repository the historical behavior is preserved: the single
    /// nearest ancestor `poly.toml` is loaded on its own (no cascade), or the
    /// default config when none is found.
    ///
    /// [`resolve_for_dir`]: PolyConfig::resolve_for_dir
    pub fn load(start: &Path) -> anyhow::Result<PolyConfig> {
        PolyConfig::load_with(start, &extends::LocalPathResolver)
    }

    /// [`load`](PolyConfig::load) with an explicit [`BaseConfigResolver`] for
    /// `extends` bases. `poly-config` itself only resolves local paths (the
    /// default [`LocalPathResolver`]); the CLI passes a resolver that also
    /// fetches pinned remote git bases.
    pub fn load_with(start: &Path, resolver: &dyn BaseConfigResolver) -> anyhow::Result<PolyConfig> {
        let dir = if start.is_file() {
            start.parent().unwrap_or(start)
        } else {
            start
        };
        if git_root(dir).is_some() {
            return PolyConfig::resolve_for_dir_with(dir, resolver);
        }
        match find_config(dir) {
            Some(path) => PolyConfig::load_file_with(&path, resolver),
            None => {
                let mut config = PolyConfig::default();
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
        PolyConfig::load_file_with(path, &extends::LocalPathResolver)
    }

    /// [`load_file`](PolyConfig::load_file) with an explicit
    /// [`BaseConfigResolver`] for `extends` bases.
    pub fn load_file_with(path: &Path, resolver: &dyn BaseConfigResolver) -> anyhow::Result<PolyConfig> {
        let mut visited = Vec::new();
        let table = read_config_table(path, resolver, &mut visited)?;
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
        PolyConfig::resolve_for_dir_with(dir, &extends::LocalPathResolver)
    }

    /// [`resolve_for_dir`](PolyConfig::resolve_for_dir) with an explicit
    /// [`BaseConfigResolver`]. Each config in the cascade resolves its own
    /// `extends` bases (with a fresh cycle-detection set) before the ancestor
    /// chain is deep-merged.
    pub fn resolve_for_dir_with(dir: &Path, resolver: &dyn BaseConfigResolver) -> anyhow::Result<PolyConfig> {
        let mut chain: Vec<(PathBuf, toml::Table)> = Vec::new();
        let mut current = Some(dir.to_path_buf());
        while let Some(d) = current {
            if let Some(path) = config_file_in(&d) {
                let mut visited = Vec::new();
                let mut table = read_config_table(&path, resolver, &mut visited)?;
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

/// Maximum depth of a transitive `extends` chain, a backstop against runaway
/// recursion independent of cycle detection.
const MAX_EXTENDS_DEPTH: usize = 32;

/// Parse a single config file into a [`toml::Table`], rejecting a malformed
/// merge directive here — while the offending file is still named.
fn parse_config_file(path: &Path) -> anyhow::Result<toml::Table> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading config {}", path.display()))?;
    let table: toml::Table = toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
    merge::validate_directives(&table).with_context(|| format!("parsing config {}", path.display()))?;
    Ok(table)
}

/// Canonicalize `path` for cycle detection, falling back to the raw path when the
/// filesystem cannot canonicalize it.
fn cycle_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Read a config file into a [`toml::Table`], resolving its `extends` bases
/// beneath it and then deep-merging its sibling [`LOCAL_OVERRIDE_NAME`] over the
/// result.
///
/// `extends` bases are merged at the raw-table level (before typed
/// deserialization) so any subset of sections can be shared; the declaring file
/// overrides its bases, and `poly.local.toml` — which may **not** itself declare
/// `extends` — is the final layer. `visited` detects `extends` cycles.
fn read_config_table(
    path: &Path,
    resolver: &dyn BaseConfigResolver,
    visited: &mut Vec<PathBuf>,
) -> anyhow::Result<toml::Table> {
    let mut memo = BTreeMap::new();
    let mut table = resolve_config_with_extends(path, resolver, visited, &mut memo)?;
    if let Some(parent) = path.parent() {
        let override_path = parent.join(LOCAL_OVERRIDE_NAME);
        if override_path.is_file() {
            let mut override_table = parse_config_file(&override_path)?;
            let local_extends = extends::take_extends(&mut override_table)
                .with_context(|| format!("parsing `extends` in {}", override_path.display()))?;
            if !local_extends.is_empty() {
                bail!(
                    "{} must not declare `extends` (it is a machine-local override)",
                    override_path.display()
                );
            }
            merge_layer(&mut table, override_table);
        }
    }
    Ok(table)
}

/// Parse `path` and deep-merge every `extends` base beneath it (recursively),
/// returning the merged raw table. Does **not** apply `path`'s sibling
/// `poly.local.toml` — machine-local overrides never leak from a base into a
/// consumer, so only [`read_config_table`] applies them, at the top level.
fn resolve_config_with_extends(
    path: &Path,
    resolver: &dyn BaseConfigResolver,
    visited: &mut Vec<PathBuf>,
    memo: &mut BTreeMap<PathBuf, toml::Table>,
) -> anyhow::Result<toml::Table> {
    let key = cycle_key(path);
    if visited.contains(&key) {
        bail!("`extends` cycle detected at {}", path.display());
    }
    // A base reachable through more than one parent (a diamond) is resolved once
    // and cloned on re-encounter — bounding a wide `extends` DAG to linear work.
    if let Some(cached) = memo.get(&key) {
        return Ok(cached.clone());
    }
    if visited.len() >= MAX_EXTENDS_DEPTH {
        bail!("`extends` chain exceeds maximum depth of {MAX_EXTENDS_DEPTH}");
    }

    visited.push(key.clone());
    let built = build_extends_table(path, resolver, visited, memo);
    // Pop unconditionally so `visited` stays a correct DFS stack even on error.
    visited.pop();
    let result = built?;

    memo.insert(key, result.clone());
    Ok(result)
}

/// Parse `path`, resolve each of its `extends` bases (recursively, memoized), and
/// return the base chain deep-merged beneath `path`'s own table.
fn build_extends_table(
    path: &Path,
    resolver: &dyn BaseConfigResolver,
    visited: &mut Vec<PathBuf>,
    memo: &mut BTreeMap<PathBuf, toml::Table>,
) -> anyhow::Result<toml::Table> {
    let mut table = parse_config_file(path)?;
    let sources =
        extends::take_extends(&mut table).with_context(|| format!("parsing `extends` in {}", path.display()))?;
    if sources.is_empty() {
        return Ok(table);
    }
    let base_dir = path.parent().unwrap_or(path);
    let mut merged = toml::Table::new();
    for source in &sources {
        let base_path = resolver.resolve(source, base_dir).with_context(|| {
            format!(
                "resolving `extends` base {:?} of {}",
                source.display_id(),
                path.display()
            )
        })?;
        let base_table = resolve_config_with_extends(&base_path, resolver, visited, memo)?;
        merge_layer(&mut merged, base_table);
    }
    // The declaring file overrides its bases — adding to, not replacing, their
    // `exclude` lists (see [`merge`]).
    merge_layer(&mut merged, table);
    Ok(merged)
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
///
/// The effective `[discovery] exclude` is folded into the file-scoped
/// `[hooks.builtin]` hooks first (see [`merge::inherit_discovery_excludes`]), and
/// the merge directives that drove all of it are stripped — they are not part of
/// the typed schema.
fn finalize(mut table: toml::Table, typos_dir: &Path) -> anyhow::Result<PolyConfig> {
    merge::inherit_discovery_excludes(&mut table);
    merge::strip_directives(&mut table);
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

/// The git repository root at or above `dir`: the nearest ancestor directory
/// containing a `.git` entry, or `None` when `dir` is not inside a git repo.
///
/// Used to decide whether config resolution should cascade (bounded at the repo
/// root) or fall back to loading the single nearest `poly.toml`.
fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut current = Some(dir.to_path_buf());
    while let Some(d) = current {
        if d.join(".git").exists() {
            return Some(d);
        }
        current = d.parent().map(Path::to_path_buf);
    }
    None
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
