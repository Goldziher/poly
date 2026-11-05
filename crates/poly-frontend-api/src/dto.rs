//! Report types a frontend component and the host exchange for the cache, rules, config and
//! whole-project tools. The host builds them from its own state and the frontend returns them to
//! its client unchanged, so both sides name one shape.

use serde::{Deserialize, Serialize};

/// One result-cache namespace's footprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CacheNamespace {
    /// Namespace directory name (e.g. `results`, `hooks`).
    pub namespace: String,
    /// Number of cached entries in this namespace.
    pub entries: u64,
    /// On-disk size of this namespace, in bytes.
    pub bytes: u64,
}

/// Result-cache footprint (mirrors `poly cache stats`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CacheStatsReport {
    /// Cache format version this binary writes.
    pub format_version: String,
    /// Cache format version found on disk, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk_version: Option<String>,
    /// Total on-disk size across all namespaces, in bytes.
    pub total_bytes: u64,
    /// Per-namespace footprint.
    pub per_namespace: Vec<CacheNamespace>,
}

/// Freed-byte report for `poly cache clean`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CacheCleanReport {
    /// Bytes reclaimed by removing every cached entry.
    pub freed_bytes: u64,
}

/// One resolved ast-grep rule — from poly's built-in pack or a user rule dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RuleInfo {
    /// Rule id (the `code` a diagnostic reports).
    pub id: String,
    /// Target language name.
    pub language: String,
    /// `builtin` for a rule from poly's embedded pack, `user` for a rule loaded
    /// from `[rules] dirs`.
    pub source: String,
    /// The severity the rule's own definition declares: `error`, `warning`,
    /// `info`, `hint`, or `off` for a rule that ships opt-in.
    pub default_severity: String,
    /// The severity a finding reports at under the resolved config, or `off`
    /// when the rule does not run.
    pub severity: String,
    /// Whether the rule participates in a scan under the resolved config.
    pub enabled: bool,
}

/// One rule-test snippet outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RuleTestOutcome {
    /// The rule id the snippet was checked against.
    pub rule_id: String,
    /// Snippet expectation: `valid`, `invalid`, or `fixed`.
    pub kind: String,
    /// Index of the snippet within its list.
    pub index: usize,
    /// Whether the snippet met its expectation.
    pub passed: bool,
    /// Failure detail, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Rule inventory and optional test report (mirrors `poly rules`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RulesReport {
    /// Rule directories that were searched for user rules.
    pub dirs: Vec<String>,
    /// Whether poly's built-in rule pack is enabled (`[rules] builtin`) — why a
    /// listing may hold no `builtin` rules at all.
    pub builtin_pack_enabled: bool,
    /// Every resolved rule, built-in pack and user rules alike, active or not.
    pub rules: Vec<RuleInfo>,
    /// Per-snippet test outcomes, present only when `test` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<Vec<RuleTestOutcome>>,
    /// Rules discovered under the searched dirs (only when `test` was requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_rules: Option<usize>,
    /// Test files naming an unknown rule id (only when `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_rule_ids: Option<Vec<String>>,
    /// Rules with no test file (only when `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untested_rule_ids: Option<Vec<String>>,
    /// Whether every snippet passed and every test named a real rule (only when
    /// `test` was requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

/// Effective `[defaults]` section (mirrors `poly config show`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ConfigDefaults {
    /// Opinionated line length.
    pub line_length: usize,
    /// Line ending policy (debug rendering of the enum).
    pub line_ending: String,
    /// Whether a trailing final newline is enforced.
    pub final_newline: bool,
    /// Whether trailing whitespace is trimmed.
    pub trim_trailing_whitespace: bool,
}

/// The effective, merged config summary (mirrors `poly config show`, without
/// remote-`extends` fetching — see the network-free `ops::resolve_poly_config`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ConfigShowReport {
    /// Resolved config file path.
    pub config_path: String,
    /// Effective defaults.
    pub defaults: ConfigDefaults,
    /// Configured `[lint.<lang>.<tool>]` top-level keys.
    pub lint_keys: Vec<String>,
    /// Configured `[fmt.<lang>.<tool>]` top-level keys.
    pub fmt_keys: Vec<String>,
    /// Opted-in catalog tool names.
    pub tools: Vec<String>,
    /// Whether a `[hooks]` section is present.
    pub hooks_present: bool,
    /// Custom ast-grep rule directories.
    pub rule_dirs: Vec<String>,
}

/// One whole-project tool's outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct WorkspaceToolReport {
    /// Hook id (e.g. `cargo-clippy`) or inline job label.
    pub id: String,
    /// Whether the tool reported a failure.
    pub failed: bool,
    /// Whether the result was served from cache.
    pub cached: bool,
    /// The tool's captured combined output (lossily decoded UTF-8).
    pub output: String,
}

/// Whole-project (workspace) lint outcome (mirrors `poly lint`'s whole-project
/// phase). Produced by the long-running `workspace_lint` / `workspace_lint_fix`
/// Task tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct WorkspaceReport {
    /// Overall pass/fail: `false` means a whole-project tool reported failures.
    pub passed: bool,
    /// Whether autofixes were applied (`workspace_lint_fix`).
    pub fixed: bool,
    /// Per-tool results, in run order. Empty when the phase did not run.
    pub tools: Vec<WorkspaceToolReport>,
}

/// One downloadable component as the host sees it (the `components` host service).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentSummary {
    /// Component name.
    pub name: String,
    /// `engines` or `frontend`.
    pub kind: String,
    /// `bundled`, `installed`, `not installed` or `unavailable`.
    pub state: String,
    /// Why the component is unavailable, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One component in a [`ComponentsStatus`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentRow {
    /// Component name.
    pub name: String,
    /// `engines` or `frontend`.
    pub kind: String,
    /// `bundled`, `installed`, `not installed` or `unavailable`.
    pub state: String,
    /// Installed only when asked for, never by the `all` preset.
    pub opt_in: bool,
    /// The engines it hosts.
    pub engines: Vec<String>,
    /// Whether the paths asked about need it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed: Option<bool>,
    /// Size on disk of the installed archive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Why it is unavailable, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What the `components` tool reports (mirrors `poly components status`). Never downloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentsStatus {
    /// `bundled` (every engine component linked), `core` (none) or `partial`.
    pub flavor: String,
    /// Whether the host is offline.
    pub offline: bool,
    /// Where components are cached.
    pub cache_root: String,
    /// Digest of the component lock this build embeds, or null when it has none.
    pub components_lock: Option<String>,
    /// Why no component lock is available, when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_error: Option<String>,
    /// The components: every one in the index, or only those the paths need when paths were given.
    pub components: Vec<ComponentRow>,
    /// Needed components that are not usable (only when paths were given).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<String>>,
    /// Needed components a run under the requested install policy would download now: missing,
    /// downloadable, and neither offline nor `install = never`.
    #[serde(default)]
    pub would_download: Vec<String>,
}

/// What happened to one component in an install or uninstall.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentAction {
    /// The component, or the cached object a prune names.
    pub name: String,
    /// `installed`, `already_installed`, `would_install`, `bundled`, `removed`, `not_installed`,
    /// `would_remove`, `skipped`, `refused` or `failed`.
    pub action: String,
    /// The reason, for a failure, a refusal or a skipped removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Outcome of `components_install` / `components_uninstall` (mirrors the CLI's action documents).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ComponentActions {
    /// `install` or `uninstall`.
    pub command: String,
    /// Whether the host is offline.
    pub offline: bool,
    /// Whether every component came out usable (or removed, for an uninstall).
    pub ok: bool,
    /// What happened to each.
    pub results: Vec<ComponentAction>,
    /// Bytes an uninstall freed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freed_bytes: Option<u64>,
    /// Remarks for a human.
    #[serde(default)]
    pub notes: Vec<String>,
}
