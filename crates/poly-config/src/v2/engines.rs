//! `[engines.<id>]` — one table per engine, replacing v1's
//! `[lint|fmt.<lang>.<tool>]` fan-out.
//!
//! Five names are reserved inside an engine table and are never engine options:
//! `enabled`, `lint`, `format`, `lang` and `rules`. Everything else is an engine
//! option, captured verbatim in `options` and validated by the engine's own
//! declared key set (`unknown-config-key`), not by this crate.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::FreeTable;

/// Names with a structural meaning inside an `[engines.<id>]` table (and, bar
/// `lang`, inside `[engines.<id>.lang.<l>]`). An engine must not declare an
/// option key with one of these names.
///
/// `enabled` is the one deliberate overlap: engines that already read an
/// `enabled` boolean as their on/off toggle keep it, and it means the same thing
/// here. `rules` is likewise already a v1 option on engines that take a rule
/// allow-list; v2 uses the `rules` *table* form (`[engines.<id>.rules.<code>]`)
/// for per-rule overrides, and an engine's array-form `rules` option has to be
/// migrated to `select`.
pub const RESERVED_ENGINE_KEYS: &[&str] = &["enabled", "format", "lang", "lint", "rules"];

/// A phase-narrowing table: `[engines.<id>.lint]` or `[engines.<id>.format]`.
/// Options set here apply to that phase only and win over the engine table.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct PhaseTable {
    /// Turn the engine off for this phase only.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Engine options for this phase.
    #[serde(flatten)]
    #[schemars(with = "FreeTable")]
    pub options: toml::Table,
}

/// A language-narrowing table: `[engines.<id>.lang.<language>]`, with optional
/// per-phase sub-tables for the language+phase combination.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct LangTable {
    /// Turn the engine off for this language only.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `[engines.<id>.lang.<language>.lint]`.
    #[serde(default)]
    pub lint: Option<PhaseTable>,
    /// `[engines.<id>.lang.<language>.format]`.
    #[serde(default)]
    pub format: Option<PhaseTable>,
    /// Engine options for this language.
    #[serde(flatten)]
    #[schemars(with = "FreeTable")]
    pub options: toml::Table,
}

/// A per-rule override: `[engines.<id>.rules.<code>]`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct RuleOverride {
    /// Severity or `"off"` for this rule (`"error"`, `"warn"`, `"off"`, ...).
    #[serde(default)]
    pub level: Option<String>,
    /// Rule parameters, passed through to the engine.
    #[serde(flatten)]
    #[schemars(with = "FreeTable")]
    pub options: toml::Table,
}

/// One `[engines.<id>]` table.
///
/// Precedence, lowest to highest: poly defaults, native config files, this
/// table's `options`, then the `lint` / `format` phase table, the `lang.<id>`
/// table, and the `lang.<id>.lint|format` table.
///
/// `[engines.astgrep]` absorbs v1's `[rules]`: its `rule_dirs` (directories of
/// user ast-grep YAML rules, relative to the config file) and `builtin_pack`
/// (whether the embedded rule pack is loaded) are ordinary options here, declared
/// by the engine like any other.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
pub struct EngineTable {
    /// `true` / `false` to switch the engine on or off; absent keeps its default
    /// (`typos`, for one, is opt-in in v2).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `[engines.<id>.lint]` — options for the lint phase only.
    #[serde(default)]
    pub lint: Option<PhaseTable>,
    /// `[engines.<id>.format]` — options for the format phase only.
    #[serde(default)]
    pub format: Option<PhaseTable>,
    /// `[engines.<id>.lang.<language>]` — per-language narrowing.
    #[serde(default)]
    pub lang: BTreeMap<String, LangTable>,
    /// `[engines.<id>.rules.<code>]` — per-rule overrides.
    #[serde(default)]
    pub rules: BTreeMap<String, RuleOverride>,
    /// Every other key: the engine's own options.
    #[serde(flatten)]
    #[schemars(with = "FreeTable")]
    pub options: toml::Table,
}
