//! Shared uniform rule-selection config schema for polylint backends.
//!
//! Backends that support rule selection, ignoring, and per-rule overrides can
//! parse their `[lint.<lang>.<tool>]` options table through
//! [`RuleSelection::from_options`].
//!
//! # Keys
//!
//! | Key | Type | Purpose |
//! |-----|------|---------|
//! | `select` | `[String]` | Replace the default rule set (codes / category names) |
//! | `extend_select` | `[String]` | Add to the default rule set |
//! | `ignore` | `[String]` | Remove from the active set |
//! | `[rules.<id>]` | table | Per-rule overrides (`level`, tool-specific params) |
//!
//! ## Level values
//!
//! `"error"`, `"warning"` (or `"warn"`), `"info"` (or `"information"`),
//! `"hint"` (or `"help"`).  An unrecognised value silently leaves `level` as
//! `None` (the engine uses its own default).

use std::collections::BTreeMap;

use crate::config::EngineConfig;
use crate::engine::Severity;

// ── Per-rule overrides ────────────────────────────────────────────────────────

/// Per-rule override from `[lint.<lang>.<tool>.rules.<id>]`.
#[derive(Debug, Clone, Default)]
pub struct RuleOptions {
    /// Severity override.  `None` keeps the tool's own default level.
    pub level: Option<Severity>,
    /// Remaining tool-specific params as a raw TOML table (all keys other than
    /// `level`).
    pub params: toml::Table,
}

// ── RuleSelection ─────────────────────────────────────────────────────────────

/// Parsed rule selection from a `[lint.<lang>.<tool>]` options table.
#[derive(Debug, Clone, Default)]
pub struct RuleSelection {
    /// Replace the default rule set (codes / category names).
    pub select: Vec<String>,
    /// Add to the default rule set (codes / category names).
    pub extend_select: Vec<String>,
    /// Remove from the active set (codes / category names).
    pub ignore: Vec<String>,
    /// Per-rule overrides keyed by rule code.
    pub rules: BTreeMap<String, RuleOptions>,
}

impl RuleSelection {
    /// Parse from a `[lint.<lang>.<tool>]` engine-config options table.
    pub fn from_options(cfg: &EngineConfig) -> Self {
        Self {
            select: string_list_from_table(&cfg.options, "select"),
            extend_select: string_list_from_table(&cfg.options, "extend_select"),
            ignore: string_list_from_table(&cfg.options, "ignore"),
            rules: parse_rules_sub_table(&cfg.options),
        }
    }

    /// `true` when no selection config was provided (fast path: keep defaults).
    pub fn is_empty(&self) -> bool {
        self.select.is_empty()
            && self.extend_select.is_empty()
            && self.ignore.is_empty()
            && self.rules.is_empty()
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Read an array-of-strings option from an engine config by key.
///
/// Returns an empty `Vec` when the key is absent or has a non-array value.
/// Shared across backends; prefer this over duplicating the same pattern.
pub(crate) fn string_list(cfg: &EngineConfig, key: &str) -> Vec<String> {
    string_list_from_table(&cfg.options, key)
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn string_list_from_table(table: &toml::Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_rules_sub_table(options: &toml::Table) -> BTreeMap<String, RuleOptions> {
    let Some(rules_val) = options.get("rules") else {
        return BTreeMap::new();
    };
    let Some(rules_table) = rules_val.as_table() else {
        return BTreeMap::new();
    };
    rules_table
        .iter()
        .filter_map(|(code, val)| {
            let sub = val.as_table()?;
            let mut opts = RuleOptions::default();
            if let Some(level_str) = sub.get("level").and_then(toml::Value::as_str) {
                opts.level = parse_level(level_str);
            }
            for (k, v) in sub {
                if k != "level" {
                    opts.params.insert(k.clone(), v.clone());
                }
            }
            Some((code.clone(), opts))
        })
        .collect()
}

/// Parse a level string to [`Severity`].  Returns `None` for unrecognised
/// strings so the caller can fall back to the tool's own default.
fn parse_level(s: &str) -> Option<Severity> {
    match s.to_lowercase().as_str() {
        "error" => Some(Severity::Error),
        "warning" | "warn" => Some(Severity::Warning),
        "info" | "information" => Some(Severity::Info),
        "hint" | "help" => Some(Severity::Hint),
        _ => None,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    fn make_cfg(toml_str: &str) -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(toml_str).expect("valid TOML"),
        }
    }

    #[test]
    fn empty_options_gives_empty_selection() {
        let sel = RuleSelection::from_options(&make_cfg(""));
        assert!(sel.is_empty());
    }

    #[test]
    fn parses_select_ignore_extend_select() {
        let cfg = make_cfg(
            r#"
select        = ["correctness", "no-eval"]
ignore        = ["no-goto"]
extend_select = ["security"]
"#,
        );
        let sel = RuleSelection::from_options(&cfg);
        assert_eq!(sel.select, vec!["correctness", "no-eval"]);
        assert_eq!(sel.ignore, vec!["no-goto"]);
        assert_eq!(sel.extend_select, vec!["security"]);
        assert!(sel.rules.is_empty());
    }

    #[test]
    fn parses_rule_level_and_extra_params() {
        let cfg = make_cfg(
            r#"
[rules.cyclomatic-complexity]
level     = "warning"
threshold = 10
"#,
        );
        let sel = RuleSelection::from_options(&cfg);
        let opts = sel
            .rules
            .get("cyclomatic-complexity")
            .expect("rule entry present");
        assert_eq!(opts.level, Some(Severity::Warning));
        assert!(opts.params.contains_key("threshold"));
    }

    #[test]
    fn unknown_level_string_leaves_none() {
        let cfg = make_cfg(
            r#"
[rules.no-eval]
level = "nonsense"
"#,
        );
        let sel = RuleSelection::from_options(&cfg);
        assert_eq!(sel.rules.get("no-eval").unwrap().level, None);
    }

    #[test]
    fn all_level_variants_parse_correctly() {
        for (s, expected) in [
            ("error", Severity::Error),
            ("warning", Severity::Warning),
            ("warn", Severity::Warning),
            ("info", Severity::Info),
            ("information", Severity::Info),
            ("hint", Severity::Hint),
            ("help", Severity::Hint),
        ] {
            assert_eq!(
                parse_level(s),
                Some(expected),
                "parse_level({s:?}) should return Some({expected:?})"
            );
        }
    }
}
