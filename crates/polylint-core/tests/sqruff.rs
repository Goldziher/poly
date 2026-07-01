//! insta snapshot fixtures for the sqruff backend.
//!
//! Two fixture categories:
//! - known-bad: a SQL file with known violations → asserts the `Diagnostic` list.
//! - known-unformatted: a SQL file sqruff can reformat → asserts exact formatted output.

use std::path::PathBuf;

use polylint_core::config::{Config, EngineConfig, GlobalDefaults, Kind};
use polylint_core::engine::{Diagnostic, Engine, SourceFile};
use polylint_core::engines::sqruff::SqruffEngine;
use polylint_core::language::Language;

fn make_source(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language: Language::Sql,
        content: content.into(),
    }
}

fn lint_cfg() -> polylint_core::config::EngineConfig {
    Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint)
}

fn fmt_cfg() -> polylint_core::config::EngineConfig {
    Config::default().engine_config(&Language::Sql, "sqruff", Kind::Format)
}

/// Build an `EngineConfig` whose options table holds a single string-array key.
fn cfg_with_codes(key: &str, codes: &[&str]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert(
        key.to_string(),
        toml::Value::Array(
            codes
                .iter()
                .map(|c| toml::Value::String((*c).into()))
                .collect(),
        ),
    );
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

/// Sorted, de-duplicated rule codes present in a diagnostic set (drops `None`).
fn sorted_codes(diags: &[Diagnostic]) -> Vec<String> {
    let mut codes: Vec<String> = diags.iter().filter_map(|d| d.code.clone()).collect();
    codes.sort();
    codes.dedup();
    codes
}

// --- known-bad fixture -------------------------------------------------------
//
// SQL with a missing space after a comma; sqruff's core ruleset flags this
// via LT01 (layout whitespace).

const KNOWN_BAD: &str = "select id,name from users\n";

#[test]
fn sqruff_known_bad_diagnostics() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", KNOWN_BAD);
    let diags = engine.lint(&src, &lint_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected violations for known-bad SQL");
    insta::assert_debug_snapshot!("sqruff_known_bad", diags);
}

// --- known-unformatted fixture ------------------------------------------------
//
// SQL with spacing issues that sqruff can autofix.

const KNOWN_UNFORMATTED: &str = "select id , name from  users where id=1\n";

#[test]
fn sqruff_known_unformatted_format() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", KNOWN_UNFORMATTED);
    let out = engine.format(&src, &fmt_cfg()).unwrap();
    assert!(
        !matches!(out, polylint_core::engine::FormatOutput::Unchanged),
        "expected formatted output for known-unformatted SQL"
    );
    if let polylint_core::engine::FormatOutput::Formatted(ref formatted) = out {
        insta::assert_snapshot!("sqruff_known_unformatted", formatted);
    }
}

// --- idempotency check -------------------------------------------------------

const WELL_FORMED: &str = "SELECT id, name\nFROM users\nWHERE id = 1\n";

#[test]
fn sqruff_format_already_formatted_is_unchanged() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", WELL_FORMED);
    let out = engine.format(&src, &fmt_cfg()).unwrap();
    if let polylint_core::engine::FormatOutput::Formatted(ref fixed) = out {
        let src2 = make_source("test.sql", fixed);
        let out2 = engine.format(&src2, &fmt_cfg()).unwrap();
        assert!(
            matches!(out2, polylint_core::engine::FormatOutput::Unchanged),
            "sqruff format must be idempotent"
        );
    }
}

// --- parse-error severity fixture --------------------------------------------
//
// Completely invalid SQL triggers a parse/lex error — sqruff emits a violation
// with the sentinel rule code "????" (no rule attached).  These must be mapped
// to Severity::Error (not Warning) and have code == None.

// An unclosed parenthesis is a genuine parse error (sqruff emits a single
// unparsable-segment diagnostic with no rule code), unlike lexable-but-invalid
// SQL which only trips layout rules.
const BROKEN_SQL: &str = "SELECT (\n";

#[test]
fn sqruff_parse_error_yields_error_severity() {
    use polylint_core::engine::Severity;

    let engine = SqruffEngine;
    let src = make_source("broken.sql", BROKEN_SQL);
    let diags = engine.lint(&src, &lint_cfg()).unwrap();

    let parse_errors: Vec<_> = diags.iter().filter(|d| d.code.is_none()).collect();
    assert!(
        !parse_errors.is_empty(),
        "expected at least one parse-error diagnostic (code=None) for broken SQL; \
         got: {diags:#?}"
    );
    assert!(
        parse_errors.iter().all(|d| d.severity == Severity::Error),
        "parse-error diagnostics must have Error severity; got: {parse_errors:#?}"
    );
}

// --- fix-capability fixture --------------------------------------------------
//
// sqruff's autofix edits are not wired through the polylint Edit path; the
// fix capability must be false so `poly lint --fix` does not silently no-op.

#[test]
fn sqruff_capabilities_fix_is_false() {
    let engine = SqruffEngine;
    let caps = engine.capabilities();
    assert!(
        !caps.fix,
        "sqruff fix capability must be false (autofix edits are not wired \
         through the polylint Edit path)"
    );
}

// --- canonical rule-selection vocabulary -------------------------------------
//
// ADR 0016: sqruff must accept the canonical `select` / `ignore` keys in
// addition to its native `rules` (allow-list) / `exclude_rules` (deny-list).
// This snippet trips two rules under the default ruleset: LT01 (missing space
// after the comma) and CP01 (inconsistent keyword capitalisation — `SELECT`
// upper, `from` lower). Two findings make allow-listing observable.

const COMMA_SQL: &str = "SELECT id,name from users\n";

#[test]
fn canonical_select_matches_native_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", COMMA_SQL);

    let native = engine
        .lint(&src, &cfg_with_codes("rules", &["LT01"]))
        .unwrap();
    let canonical = engine
        .lint(&src, &cfg_with_codes("select", &["LT01"]))
        .unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `select` must behave like native `rules`"
    );
    assert_eq!(
        sorted_codes(&native),
        vec!["LT01".to_string()],
        "allow-listing LT01 must narrow the findings to LT01 only; got: {native:#?}"
    );
}

#[test]
fn canonical_ignore_matches_native_exclude_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", COMMA_SQL);

    let native = engine
        .lint(&src, &cfg_with_codes("exclude_rules", &["LT01"]))
        .unwrap();
    let canonical = engine
        .lint(&src, &cfg_with_codes("ignore", &["LT01"]))
        .unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `ignore` must behave like native `exclude_rules`"
    );
    assert!(
        !sorted_codes(&native).contains(&"LT01".to_string()),
        "excluding LT01 must suppress it; got: {native:#?}"
    );
}

// --- per-rule parameter fixture: capitalisation policy -----------------------
//
// Proves that `rule_configs."capitalisation.keywords" = { capitalisation_policy
// = "upper" }` changes lint findings vs the default `consistent` policy.
// Default (consistent): all-lowercase SQL has no CP01 violation because the
// capitalisation is internally consistent.
// With "upper": lowercase keywords `select` / `from` violate CP01.

const LOWERCASE_SQL: &str = "select a, b from users\n";

#[test]
fn sqruff_per_rule_param_capitalisation_policy_upper() {
    use polylint_core::config::{EngineConfig, GlobalDefaults};

    let engine = SqruffEngine;
    let src = make_source("test.sql", LOWERCASE_SQL);

    // Baseline: default config (consistent) should not flag all-lowercase SQL.
    let default_diags = engine.lint(&src, &lint_cfg()).unwrap();
    let cp01_default = default_diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("CP01"))
        .count();
    assert_eq!(
        cp01_default, 0,
        "consistent policy should not flag all-lowercase SQL; got: {default_diags:#?}"
    );

    // With capitalisation_policy = "upper": lowercase keywords must be flagged.
    let mut cap_opts = toml::Table::new();
    cap_opts.insert(
        "capitalisation_policy".to_string(),
        toml::Value::String("upper".to_string()),
    );
    let mut rule_configs = toml::Table::new();
    rule_configs.insert(
        "capitalisation.keywords".to_string(),
        toml::Value::Table(cap_opts),
    );
    let mut options = toml::Table::new();
    options.insert("rule_configs".to_string(), toml::Value::Table(rule_configs));

    let upper_cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };

    let upper_diags = engine.lint(&src, &upper_cfg).unwrap();
    assert!(
        upper_diags
            .iter()
            .any(|d| d.code.as_deref() == Some("CP01")),
        "capitalisation_policy = 'upper' should flag lowercase keywords (CP01); \
         got: {upper_diags:#?}"
    );
}
