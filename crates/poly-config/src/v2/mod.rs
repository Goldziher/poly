//! The `poly.toml` **v2** schema (ADR 0038).
//!
//! Engine-first: one `[engines.<id>]` table per engine replaces the v1
//! `[lint|fmt.<lang>.<tool>]` fan-out, `[components]` selects downloadable engine
//! components, and `required_version` / `root` / `[workspace] run` /
//! `per_file_ignores` replace their v1 spellings.
//!
//! This module is additive: the loader still reads v1. [`detect_v1`] and
//! [`PolyConfigV2`] are wired into loading by the engine-first resolution work.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{CacheConfig, CommitConfig, DiscoveryConfig, HooksConfig, RawDefaults, ToolsConfig};

mod components;
mod detect;
mod engines;
mod paths;
mod workspace;

pub use components::{ComponentInstallPolicy, ComponentsConfig, Enable, OnMissing};
pub use detect::{V1Marker, detect_v1, v1_error};
pub use engines::{EngineTable, LangTable, PhaseTable, RESERVED_ENGINE_KEYS, RuleOverride};
pub use paths::{BUILTIN_PACK_KEY, RULE_DIRS_KEY};
pub(crate) use paths::{reject_mixed, resolve_rule_dirs_in_raw, resolve_rule_dirs_typed};
pub use workspace::{RunMode, WorkspaceV2};

/// Every key v2 `poly.toml` recognises at the top level, sorted.
///
/// `extends` and `exclude_mode` are consumed by the merge layer but are valid in
/// a file, so they are listed. Held to the typed schema by tests.
pub const V2_TOP_LEVEL_KEYS: &[&str] = &[
    "cache",
    "commit",
    "components",
    "defaults",
    "discovery",
    "engines",
    "exclude_mode",
    "extends",
    "hooks",
    "per_file_ignores",
    "required_version",
    "root",
    "tools",
    "workspace",
];

/// The parsed v2 `poly.toml`. Unknown top-level keys are a parse error.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[schemars(
    title = "poly.toml (v2)",
    description = "The poly.toml v2 configuration document: engines, components, workspace, hooks and commit."
)]
pub struct PolyConfigV2 {
    /// Semver requirement on the running `poly` (`">=2.0, <3"`).
    pub required_version: Option<String>,
    /// Stop the upward config cascade here (replaces v1 `[workspace] root`).
    pub root: bool,
    /// Base configs to inherit from (ADR 0020): a path or `{ path | git, revision, file }`,
    /// single or array. Resolved by the merge layer before typed parsing.
    #[schemars(with = "crate::ExtendsSchema")]
    pub extends: Option<toml::Value>,
    /// `"replace"` makes `[discovery] exclude` replace inherited globs instead of
    /// accumulating them.
    pub exclude_mode: Option<String>,
    /// `[components]` — component selection and install policy.
    pub components: ComponentsConfig,
    /// `[workspace]` — whole-project phase control.
    pub workspace: WorkspaceV2,
    /// `[engines.<id>]` — one table per engine.
    pub engines: BTreeMap<String, EngineTable>,
    /// `[tools.<name>]` — opted-in catalog tools (unchanged from v1).
    pub tools: ToolsConfig,
    /// `[per_file_ignores]` — path glob to rule codes to suppress.
    pub per_file_ignores: BTreeMap<String, Vec<String>>,
    /// `[hooks]` — `poly hooks` configuration (unchanged from v1).
    pub hooks: HooksConfig,
    /// `[commit]` — `poly commit` configuration (unchanged from v1).
    pub commit: CommitConfig,
    /// `[cache]` — result-cache configuration (unchanged from v1).
    pub cache: CacheConfig,
    /// `[defaults]` — opinionated global defaults (unchanged from v1).
    pub defaults: RawDefaults,
    /// `[discovery]` — file-walk tuning (unchanged from v1).
    pub discovery: DiscoveryConfig,
}

/// The JSON Schema for the v2 `poly.toml`. Committed as
/// `schemas/poly-v2.schema.json`; engine option keys are not enumerated here
/// (each engine declares its own).
#[must_use]
pub fn config_schema_v2() -> schemars::Schema {
    schemars::schema_for!(PolyConfigV2)
}
