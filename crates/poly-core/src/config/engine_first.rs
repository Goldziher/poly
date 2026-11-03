//! Engine-first option resolution for the v2 `[engines.<id>]` tables (ADR 0038).
//!
//! A v2 engine table is flattened into an ordered list of *layers*, lowest
//! precedence first:
//!
//! 1. `[engines.<id>]`
//! 2. `[engines.<id>.lint]` / `[engines.<id>.format]` for the phase being resolved
//! 3. `[engines.<id>.lang.<language>]`
//! 4. `[engines.<id>.lang.<language>.lint]` / `.format`
//!
//! Tool defaults and native config files sit beneath all four and are applied by
//! [`Config::engine_config`](super::Config::engine_config) exactly as for v1.
//!
//! The layers are the inverse of the `poly migrate --to-v2` placement: a v1 table
//! that the importer hoisted to a higher level resolves to the same options here,
//! because every level the importer writes to is read back in the order the
//! importer relied on.

use std::collections::BTreeMap;

use poly_config::v2::{BUILTIN_PACK_KEY, EngineTable, PhaseTable, RULE_DIRS_KEY, RuleOverride};

use super::Kind;
use crate::engine::ENABLED_OPTION_KEY;

/// The table of per-rule overrides inside a layer (`rules.<id>`).
const RULES_KEY: &str = "rules";

/// The phase sub-table that applies to `kind`.
fn phase_of<'a>(lint: &'a Option<PhaseTable>, format: &'a Option<PhaseTable>, kind: Kind) -> Option<&'a PhaseTable> {
    match kind {
        Kind::Lint => lint.as_ref(),
        Kind::Format => format.as_ref(),
    }
}

/// A rule override as the flat table v1 engines already read: `level` plus its parameters.
fn rule_table(rule: &RuleOverride) -> toml::Table {
    let mut table = rule.options.clone();
    if let Some(level) = &rule.level {
        table.insert("level".to_string(), toml::Value::String(level.clone()));
    }
    table
}

/// One layer: the options, the `enabled` toggle under its option key, and any
/// typed `rules` overrides as a `rules` table.
fn layer(enabled: Option<bool>, options: &toml::Table, rules: Option<&BTreeMap<String, RuleOverride>>) -> toml::Table {
    let mut out = options.clone();
    if let Some(rules) = rules.filter(|rules| !rules.is_empty()) {
        let table: toml::Table = rules
            .iter()
            .map(|(id, rule)| (id.clone(), toml::Value::Table(rule_table(rule))))
            .collect();
        out.insert(RULES_KEY.to_string(), toml::Value::Table(table));
    }
    if let Some(enabled) = enabled {
        out.insert(ENABLED_OPTION_KEY.to_string(), toml::Value::Boolean(enabled));
    }
    out
}

/// The option layers of `engine` for `language` in `kind`, lowest precedence first.
/// Empty when `[engines.<engine>]` is absent.
pub(super) fn layers(
    engines: &BTreeMap<String, EngineTable>,
    engine: &str,
    language: &str,
    kind: Kind,
) -> Vec<toml::Table> {
    let Some(table) = engines.get(engine) else {
        return Vec::new();
    };
    let mut out = vec![layer(table.enabled, &table.options, Some(&table.rules))];
    if let Some(phase) = phase_of(&table.lint, &table.format, kind) {
        out.push(layer(phase.enabled, &phase.options, None));
    }
    if let Some(lang) = table.lang.get(language) {
        out.push(layer(lang.enabled, &lang.options, None));
        if let Some(phase) = phase_of(&lang.lint, &lang.format, kind) {
            out.push(layer(phase.enabled, &phase.options, None));
        }
    }
    drop_lint_only_rules(&mut out, engine, kind);
    out
}

/// `rules.<id>` overrides carry no phase in v2 and are lint settings. The format
/// phase reads them only for an engine that has no lint phase, mirroring where the
/// `poly migrate --to-v2` importer takes them from; otherwise a rule override would
/// leak into a formatter's options.
fn drop_lint_only_rules(layers: &mut [toml::Table], engine: &str, kind: Kind) {
    if kind != Kind::Format || !layers.iter().any(|layer| layer.contains_key(RULES_KEY)) {
        return;
    }
    if crate::registry::engine_languages(engine, false).is_empty() {
        return;
    }
    for layer in layers {
        layer.remove(RULES_KEY);
    }
}

/// Fold `layers` into one table: a later layer's key replaces an earlier one's, except
/// `rules`, whose per-rule tables merge key by key so a language can adjust one
/// parameter of an engine-level rule override without restating it.
///
/// Arrays are replaced here, never concatenated; the engines that union arrays
/// (typos, uncomment) do so in their own option builders.
pub(super) fn merge<'a>(layers: impl IntoIterator<Item = &'a toml::Table>) -> toml::Table {
    let mut out = toml::Table::new();
    for layer in layers {
        for (key, value) in layer {
            if key == RULES_KEY
                && let (Some(toml::Value::Table(previous)), toml::Value::Table(next)) = (out.get_mut(key), value)
            {
                merge_rules(previous, next);
                continue;
            }
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

/// Merge `next`'s per-rule tables over `previous`, parameter by parameter.
fn merge_rules(previous: &mut toml::Table, next: &toml::Table) {
    for (id, value) in next {
        match (previous.get_mut(id), value) {
            (Some(toml::Value::Table(existing)), toml::Value::Table(incoming)) => {
                for (key, parameter) in incoming {
                    existing.insert(key.clone(), parameter.clone());
                }
            }
            _ => {
                previous.insert(id.clone(), value.clone());
            }
        }
    }
}

/// The ast-grep settings v2 moved out of the top-level `[rules]` table:
/// `[engines.astgrep] rule_dirs` and `builtin_pack`, each `None` when unset.
///
/// Read at engine and lint-phase level only: the rule directories are a property
/// of the run, not of a language.
pub(super) fn astgrep_rule_settings(engines: &BTreeMap<String, EngineTable>) -> (Option<Vec<String>>, Option<bool>) {
    let merged = merge(&layers(engines, "astgrep", "", Kind::Lint));
    let dirs = merged
        .get(RULE_DIRS_KEY)
        .and_then(toml::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        });
    let builtin = merged.get(BUILTIN_PACK_KEY).and_then(toml::Value::as_bool);
    (dirs, builtin)
}
