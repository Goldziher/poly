//! insta snapshot fixtures for the sqruff backend.
//!
//! Two fixture categories:
//! - known-bad: a SQL file with known violations → asserts the `Diagnostic` list.
//! - known-unformatted: a SQL file sqruff can reformat → asserts exact formatted output.

use std::path::PathBuf;

use poly_core::config::{Config, EngineConfig, GlobalDefaults, Kind};
use poly_core::engine::{Diagnostic, Engine, SourceFile};
use poly_core::engines::sqruff::SqruffEngine;
use poly_core::language::Language;

fn make_source(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language: Language::Sql,
        content: content.into(),
    }
}

fn lint_cfg() -> poly_core::config::EngineConfig {
    Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint)
}

fn fmt_cfg() -> poly_core::config::EngineConfig {
    Config::default().engine_config(&Language::Sql, "sqruff", Kind::Format)
}

/// Build an `EngineConfig` whose options table holds a single string-array key.
fn cfg_with_codes(key: &str, codes: &[&str]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert(
        key.to_string(),
        toml::Value::Array(codes.iter().map(|c| toml::Value::String((*c).into())).collect()),
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

/// A known-bad file whose findings are *not* layout: `a b` is an implicit column
/// alias (AL02) and `= null` should be `IS NULL` (CV05). The embedded `id,name`
/// is a genuine LT01 violation that must **not** appear — `poly fmt` owns it.
const KNOWN_BAD: &str = "select a b, id,name from users where id = null\n";

#[test]
fn sqruff_known_bad_diagnostics() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", KNOWN_BAD);
    let diags = engine.lint(&src, &lint_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected violations for known-bad SQL");
    insta::assert_debug_snapshot!("sqruff_known_bad", diags);
}

const KNOWN_UNFORMATTED: &str = "select id , name from  users where id=1\n";

#[test]
fn sqruff_known_unformatted_format() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", KNOWN_UNFORMATTED);
    let out = engine.format(&src, &fmt_cfg()).unwrap();
    assert!(
        !matches!(out, poly_core::engine::FormatOutput::Unchanged),
        "expected formatted output for known-unformatted SQL"
    );
    if let poly_core::engine::FormatOutput::Formatted(ref formatted) = out {
        insta::assert_snapshot!("sqruff_known_unformatted", formatted);
    }
}

const WELL_FORMED: &str = "SELECT id, name\nFROM users\nWHERE id = 1\n";

#[test]
fn sqruff_format_already_formatted_is_unchanged() {
    let engine = SqruffEngine;
    let src = make_source("test.sql", WELL_FORMED);
    let out = engine.format(&src, &fmt_cfg()).unwrap();
    if let poly_core::engine::FormatOutput::Formatted(ref fixed) = out {
        let src2 = make_source("test.sql", fixed);
        let out2 = engine.format(&src2, &fmt_cfg()).unwrap();
        assert!(
            matches!(out2, poly_core::engine::FormatOutput::Unchanged),
            "sqruff format must be idempotent"
        );
    }
}

const BROKEN_SQL: &str = "SELECT (\n";

#[test]
fn sqruff_parse_error_yields_error_severity() {
    use poly_core::engine::Severity;

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

/// Implicit column alias (AL02) plus a comma-spacing violation (LT01). AL02 is
/// used for the rule-selection tests because LT01 is format-owned and therefore
/// never reported by `lint`, which would make those assertions vacuous.
const COMMA_SQL: &str = "SELECT a b, id,name from users\n";

#[test]
fn canonical_select_matches_native_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", COMMA_SQL);

    let native = engine.lint(&src, &cfg_with_codes("rules", &["AL02"])).unwrap();
    let canonical = engine.lint(&src, &cfg_with_codes("select", &["AL02"])).unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `select` must behave like native `rules`"
    );
    assert_eq!(
        sorted_codes(&native),
        vec!["AL02".to_string()],
        "allow-listing AL02 must narrow the findings to AL02 only; got: {native:#?}"
    );
}

#[test]
fn canonical_ignore_matches_native_exclude_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", COMMA_SQL);

    let native = engine.lint(&src, &cfg_with_codes("exclude_rules", &["AL02"])).unwrap();
    let canonical = engine.lint(&src, &cfg_with_codes("ignore", &["AL02"])).unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `ignore` must behave like native `exclude_rules`"
    );
    assert!(
        !sorted_codes(&native).contains(&"AL02".to_string()),
        "excluding AL02 must suppress it; got: {native:#?}"
    );
}

const LOWERCASE_SQL: &str = "select a, b from users\n";

/// The `rule_configs` plumbing is exercised through `format`, not `lint`:
/// capitalisation is format-owned, so CP01 never appears as a diagnostic.
#[test]
fn sqruff_per_rule_param_capitalisation_policy_upper() {
    use poly_core::config::{EngineConfig, GlobalDefaults};

    let engine = SqruffEngine;
    let src = make_source("test.sql", LOWERCASE_SQL);

    assert!(
        matches!(
            engine.format(&src, &fmt_cfg()).unwrap(),
            poly_core::engine::FormatOutput::Unchanged
        ),
        "the default `consistent` policy should leave all-lowercase SQL alone"
    );

    let mut cap_opts = toml::Table::new();
    cap_opts.insert(
        "capitalisation_policy".to_string(),
        toml::Value::String("upper".to_string()),
    );
    let mut rule_configs = toml::Table::new();
    rule_configs.insert("capitalisation.keywords".to_string(), toml::Value::Table(cap_opts));
    let mut options = toml::Table::new();
    options.insert("rule_configs".to_string(), toml::Value::Table(rule_configs));

    let upper_cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };

    match engine.format(&src, &upper_cfg).unwrap() {
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, "SELECT a, b FROM users\n",
                "capitalisation_policy = 'upper' must upper-case keywords"
            );
        }
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("capitalisation_policy = 'upper' should have rewritten the keywords")
        }
    }
}

// ---------------------------------------------------------------------------
// lint / format partition
//
// The split is presentation vs meaning: `poly fmt` applies only sqruff's
// `layout` and `capitalisation` groups, `poly lint` reports only the rest. See
// the `FORMAT_OWNED_GROUPS` doc comment in `engines/sqruff.rs`.
// ---------------------------------------------------------------------------

/// `WHERE id = NULL` and `WHERE id IS NULL` **return different rows**: in SQL's
/// three-valued logic `= NULL` evaluates to UNKNOWN and never matches, while
/// `IS NULL` matches every NULL. sqruff's CV05 (`convention.is_null`) rewrites
/// the first into the second.
///
/// A formatter that applied it would silently change what the query returns —
/// and, worse, erase the evidence of a real bug the author needs to see. This is
/// the single most important guarantee in this file: `poly fmt` must leave this
/// query byte-identical. `poly lint` reports CV05 so the author fixes it
/// deliberately (see [`sqruff_known_bad_diagnostics`]).
const MEANING_CHANGING_SQL: &str = "SELECT id\nFROM users\nWHERE id = NULL\n";

#[test]
fn format_does_not_change_which_rows_a_query_returns() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", MEANING_CHANGING_SQL);
    match engine.format(&src, &fmt_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Unchanged => {}
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, MEANING_CHANGING_SQL,
                "format rewrote `= NULL` into `IS NULL` — that changes the result set"
            );
        }
    }
}

#[test]
fn lint_still_reports_the_meaning_changing_comparison() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", MEANING_CHANGING_SQL);
    let diags = engine.lint(&src, &lint_cfg()).unwrap();
    assert_eq!(
        sorted_codes(&diags),
        vec!["CV05".to_string()],
        "lint must report the `= NULL` bug format refuses to silently repair; got: {diags:#?}"
    );
}

/// An unused table alias. AL05 (`aliasing.unused`) deletes it — a query edit
/// resting entirely on sqruff's reference detection being complete.
const UNUSED_ALIAS_SQL: &str = "SELECT b.x\nFROM tbl AS a\nJOIN other AS b ON b.x = 1\n";

#[test]
fn format_does_not_delete_an_unused_alias() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", UNUSED_ALIAS_SQL);
    match engine.format(&src, &fmt_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Unchanged => {}
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, UNUSED_ALIAS_SQL,
                "format must not delete an alias — a missed reference is a broken query"
            );
        }
    }
}

/// A subquery in a `JOIN`. ST05 (`structure.subquery`) rewrites it into a CTE —
/// a restructuring of the query, not a reformatting of it. ST05 is **not** in
/// sqruff's default `core` rule set, so it must be enabled explicitly.
const STRUCTURAL_SQL: &str = "SELECT a.x\nFROM a\nJOIN (SELECT y, z FROM b) AS b ON a.x = b.y\n";

/// Enables ST05 on top of sqruff's default `core` set.
fn structural_cfg() -> EngineConfig {
    cfg_with_codes("rules", &["core", "ST05"])
}

#[test]
fn format_does_not_restructure_a_subquery() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", STRUCTURAL_SQL);
    match engine.format(&src, &structural_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Unchanged => {}
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, STRUCTURAL_SQL,
                "format must leave a structural (ST) violation byte-identical"
            );
        }
    }
}

#[test]
fn lint_still_reports_a_restructurable_subquery() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", STRUCTURAL_SQL);
    let diags = engine.lint(&src, &structural_cfg()).unwrap();
    assert!(
        sorted_codes(&diags).contains(&"ST05".to_string()),
        "lint must still report the structural rule format refuses to apply; got: {diags:#?}"
    );
}

#[test]
fn lint_offers_no_autofix_for_a_structural_rule() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", STRUCTURAL_SQL);
    let diags = engine.lint(&src, &structural_cfg()).unwrap();
    let structural: Vec<_> = diags.iter().filter(|d| d.code.as_deref() == Some("ST05")).collect();
    assert_eq!(
        structural.len(),
        1,
        "expected exactly one ST05 diagnostic; got: {diags:#?}"
    );
    assert_eq!(
        structural[0].fix,
        vec![],
        "a structural diagnostic must carry no fix — `poly lint --fix` applies diagnostic edits"
    );
}

/// Comma spacing (LT01, layout) and a lower-case `from` among upper-case
/// keywords (CP01, capitalisation) — one violation from each format-owned group.
const FORMAT_OWNED_SQL: &str = "SELECT id,name from users\n";

/// One violation from each of the four groups that matter here: `a b` is AL02
/// (aliasing, lint-owned), `id,name` is LT01 (layout, format-owned), the
/// lower-case `where` is CP01 (capitalisation, format-owned), and `= null` is
/// CV05 (convention, lint-owned).
const ALL_FOUR_GROUPS_SQL: &str = "SELECT a b, id,name FROM users where id = null\n";

/// poly appends its group suppression to the user's `exclude_rules`; it must not
/// replace it. A user silently losing their own exclusions would be a worse bug
/// than the asymmetry this partition fixes, and the two lists are emitted on a
/// single INI line, so a clobber would be invisible without this test.
#[test]
fn user_exclude_rules_and_poly_suppression_both_apply() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", ALL_FOUR_GROUPS_SQL);

    assert_eq!(
        sorted_codes(&engine.lint(&src, &lint_cfg()).unwrap()),
        vec!["AL02".to_string(), "CV05".to_string()],
        "poly's suppression alone must drop LT01 and CP01 and keep the rest"
    );

    let diags = engine.lint(&src, &cfg_with_codes("exclude_rules", &["AL02"])).unwrap();
    assert_eq!(
        sorted_codes(&diags),
        vec!["CV05".to_string()],
        "the user's `exclude_rules` (AL02) and poly's group suppression (LT01, CP01) must \
         BOTH apply — neither may clobber the other; got: {diags:#?}"
    );

    let diags = engine.lint(&src, &cfg_with_codes("ignore", &["CV05"])).unwrap();
    assert_eq!(
        sorted_codes(&diags),
        vec!["AL02".to_string()],
        "the canonical `ignore` key must compose with poly's suppression too; got: {diags:#?}"
    );
}

/// Guards the assumption the group-level suppression rests on: excluding a group
/// name must remove exactly that group. Every sqruff rule carries the meta-groups
/// `all` and `core` alongside **one** functional group, so denying `layout` can
/// never take a capitalisation or convention rule with it. If upstream ever gives
/// a rule two functional groups, this fails and the suppression needs rethinking.
#[test]
fn every_sqruff_rule_has_exactly_one_functional_group() {
    use sqruff_lib::core::rules::RuleGroups;

    let offenders: Vec<String> = sqruff_lib::rules::rules()
        .iter()
        .filter(|rule| {
            rule.groups()
                .iter()
                .filter(|group| !matches!(group, RuleGroups::All | RuleGroups::Core))
                .count()
                != 1
        })
        .map(|rule| format!("{} -> {:?}", rule.code(), rule.groups()))
        .collect();

    assert_eq!(
        offenders,
        Vec::<String>::new(),
        "a rule in two functional groups would make group exclusion leak across groups"
    );
}

#[test]
fn lint_does_not_report_format_owned_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", FORMAT_OWNED_SQL);
    let diags = engine.lint(&src, &lint_cfg()).unwrap();
    assert_eq!(
        sorted_codes(&diags),
        Vec::<String>::new(),
        "LT01 and CP01 are format-owned and must not surface in lint; got: {diags:#?}"
    );
}

#[test]
fn format_still_fixes_layout_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", "select id,name from users\n");
    match engine.format(&src, &fmt_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, "select id, name from users\n",
                "LT01 must still be fixed by format"
            );
        }
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("expected the layout rule LT01 to be fixed by format")
        }
    }
}

#[test]
fn format_still_fixes_capitalisation_rules() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", "SELECT id, name from users\n");
    match engine.format(&src, &fmt_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(
                out, "SELECT id, name FROM users\n",
                "CP01 must still be fixed by format"
            );
        }
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("expected the capitalisation rule CP01 to be fixed by format")
        }
    }
}

/// Both format-owned groups applied in a single pass.
#[test]
fn format_fixes_layout_and_capitalisation_together() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", FORMAT_OWNED_SQL);
    match engine.format(&src, &fmt_cfg()).unwrap() {
        poly_core::engine::FormatOutput::Formatted(out) => {
            assert_eq!(out, "SELECT id, name FROM users\n");
        }
        poly_core::engine::FormatOutput::Unchanged => panic!("expected LT01 and CP01 to both be fixed"),
    }
}

#[test]
fn format_leaves_well_formed_sql_unchanged() {
    let engine = SqruffEngine;
    let src = make_source("t.sql", WELL_FORMED);
    assert!(
        matches!(
            engine.format(&src, &fmt_cfg()).unwrap(),
            poly_core::engine::FormatOutput::Unchanged
        ),
        "well-formed SQL must be reported Unchanged"
    );
}
