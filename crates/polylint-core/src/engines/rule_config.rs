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
        self.select.is_empty() && self.extend_select.is_empty() && self.ignore.is_empty() && self.rules.is_empty()
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

/// Deserialize an engine's options table into a typed config `T`.
///
/// Returns `T::default()` when `cfg.options` is empty (fast path: keep the
/// tool's own defaults).  Otherwise the options table is deserialized into `T`;
/// on a deserialization error the `context` string is logged as a warning and
/// `T::default()` is returned so a malformed user table never aborts the run.
///
/// Shared across format-only backends whose upstream `FormatOptions` type is
/// `serde`-deserializable (e.g. malva, markup_fmt); prefer this over
/// duplicating the parse-or-default pattern.
pub(crate) fn deserialize_options<T: serde::de::DeserializeOwned + Default>(cfg: &EngineConfig, context: &str) -> T {
    if cfg.options.is_empty() {
        return T::default();
    }
    toml::Value::Table(cfg.options.clone())
        .try_into()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "{context} options could not be parsed; using defaults");
            T::default()
        })
}

/// Union two rule-code lists, preserving first-seen order and dropping exact
/// duplicates.
///
/// Backends that accept both the canonical vocabulary (`select` / `ignore`) and
/// their own native aliases use this to merge the two sources without emitting a
/// code twice to the wrapped tool.
pub(crate) fn union_codes(primary: Vec<String>, extra: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut merged = Vec::new();
    for code in primary.into_iter().chain(extra) {
        if seen.insert(code.clone()) {
            merged.push(code);
        }
    }
    merged
}

/// Drop blank (empty or whitespace-only) rule codes, emitting a `warn` for each.
///
/// Backends that forward codes straight to their wrapped tool have no cheap rule
/// registry to validate against, so this is the proportionate unknown-code
/// guard: a code that cannot possibly resolve is surfaced and skipped rather
/// than silently passed through as an empty INI/list entry.
pub(crate) fn warn_and_skip_blank(codes: Vec<String>, engine: &str) -> Vec<String> {
    codes
        .into_iter()
        .filter(|code| {
            if code.trim().is_empty() {
                tracing::warn!(code = %code, engine, "unknown rule or category; skipping");
                false
            } else {
                true
            }
        })
        .collect()
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn string_list_from_table(table: &toml::Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
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
                if opts.level.is_none() {
                    tracing::warn!(
                        rule = %code,
                        level = level_str,
                        "unrecognized level in [rules.<id>]; using the rule's default severity"
                    );
                }
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
        let opts = sel.rules.get("cyclomatic-complexity").expect("rule entry present");
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

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(default)]
    struct TinyOptions {
        width: u16,
        name: String,
    }

    #[test]
    fn deserialize_options_empty_gives_default() {
        let parsed: TinyOptions = deserialize_options(&make_cfg(""), "[test]");
        assert_eq!(parsed, TinyOptions::default());
    }

    #[test]
    fn deserialize_options_valid_is_parsed() {
        let cfg = make_cfg(
            r#"
width = 120
name  = "poly"
"#,
        );
        let parsed: TinyOptions = deserialize_options(&cfg, "[test]");
        assert_eq!(
            parsed,
            TinyOptions {
                width: 120,
                name: "poly".to_owned(),
            }
        );
    }

    #[test]
    fn deserialize_options_invalid_falls_back_to_default() {
        // `width` expects an integer; a string value fails deserialization and
        // must fall back to the default without panicking.
        let cfg = make_cfg(r#"width = "not-a-number""#);
        let parsed: TinyOptions = deserialize_options(&cfg, "[test]");
        assert_eq!(parsed, TinyOptions::default());
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
