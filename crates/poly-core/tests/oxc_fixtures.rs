//! insta snapshot fixtures for the oxc backend.
//! Two kinds:
//!   1. known-bad file  → expected `Diagnostic`s
//!   2. known-unformatted file → exact formatted output

use std::path::PathBuf;

use poly_core::config::{EngineConfig, GlobalDefaults};
use poly_core::engine::{Engine, Severity, SourceFile};
use poly_core::engines::oxc::OxcEngine;
use poly_core::language::Language;

fn make_src(content: &str, path: &str, lang: Language) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language: lang,
        content: content.into(),
    }
}

fn default_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

/// Syntactically broken JS file — asserts that at least one Error-severity
/// diagnostic is produced. The exact message format is not snapshotted here
/// because it comes from oxlint's internal parser and may evolve.
#[test]
fn oxc_known_bad_js_diagnostics() {
    let src = make_src("const x = {\n  a: 1,\nconst y = 2;\n", "bad.js", Language::JavaScript);
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    assert!(
        diags.iter().any(|d| d.severity == Severity::Error),
        "expected at least one Error-severity diagnostic; got: {diags:?}"
    );
}

/// JS fixture with a `debugger` statement — asserts the `no-debugger`
/// correctness rule fires with Warning severity.
///
/// Source lives in a fixture file so prek hooks (typos, trailing-whitespace)
/// cannot silently mutate the lint-triggering literal during a pre-commit run.
#[test]
fn oxc_oxlint_no_debugger_rule() {
    let content = include_str!("fixtures/oxc/bad_js.js");
    let src = make_src(content, "bad_js.js", Language::JavaScript);
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();

    let debugger_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("no-debugger"))
        .collect();
    assert!(
        !debugger_diags.is_empty(),
        "expected a no-debugger diagnostic; got: {diags:?}"
    );
    assert_eq!(
        debugger_diags[0].severity,
        Severity::Warning,
        "no-debugger should be Warning severity"
    );

    let summary: Vec<(Option<&str>, &Severity)> = diags.iter().map(|d| (d.code.as_deref(), &d.severity)).collect();
    insta::assert_debug_snapshot!(summary);
}

/// Behavioral proof: `ignore = ["no-debugger"]` in engine config suppresses the
/// `no-debugger` rule that fires by default.
///
/// This verifies end-to-end that config options flow through `lint()` into
/// `ConfigStoreBuilder::with_filter` and actually change the rule output.
#[test]
fn oxc_config_ignore_suppresses_rule() {
    let content = include_str!("fixtures/oxc/bad_js.js");
    let src = make_src(content, "bad_js.js", Language::JavaScript);
    let engine = OxcEngine;

    let default_diags = engine.lint(&src, &default_cfg()).unwrap();
    assert!(
        default_diags.iter().any(|d| d.code.as_deref() == Some("no-debugger")),
        "expected no-debugger with default config; got: {default_diags:?}"
    );

    let mut opts = toml::Table::new();
    opts.insert(
        "ignore".to_owned(),
        toml::Value::Array(vec![toml::Value::String("no-debugger".to_owned())]),
    );
    let cfg_ignore = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: opts,
    };
    let ignore_diags = engine.lint(&src, &cfg_ignore).unwrap();
    assert!(
        !ignore_diags.iter().any(|d| d.code.as_deref() == Some("no-debugger")),
        "no-debugger should be suppressed via ignore config; got: {ignore_diags:?}"
    );
}

/// JSON with a trailing comma — asserts the expected parse-error Diagnostic.
#[test]
fn oxc_known_bad_json_diagnostics() {
    let src = make_src("{\n  \"a\": 1,\n  \"b\": 2,\n}\n", "bad.json", Language::Json);
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected at least one diagnostic for trailing comma");
    insta::assert_debug_snapshot!(diags[0].title);
}

/// Compact JS file → asserts exact Prettier-compatible output from oxc_formatter.
#[test]
fn oxc_known_unformatted_js_output() {
    let src = make_src(
        "const x={a:1,b:2};\nfunction foo(a,b){return a+b;}\n",
        "ugly.js",
        Language::JavaScript,
    );
    let out = OxcEngine.format(&src, &default_cfg()).unwrap();
    match out {
        poly_core::engine::FormatOutput::Formatted(text) => {
            insta::assert_snapshot!(text);
        }
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("expected Formatted, got Unchanged");
        }
    }
}

/// Compact JSON file → asserts exact pretty-printed output.
#[test]
fn oxc_known_unformatted_json_output() {
    let src = make_src(r#"{"b":2,"a":1}"#, "ugly.json", Language::Json);
    let out = OxcEngine.format(&src, &default_cfg()).unwrap();
    match out {
        poly_core::engine::FormatOutput::Formatted(text) => {
            insta::assert_snapshot!(text);
        }
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("expected Formatted, got Unchanged");
        }
    }
}

/// JSONC with comments is valid (no diagnostics).
#[test]
fn oxc_jsonc_with_comments_is_clean() {
    let src = make_src(
        "{\n  // comment\n  \"key\": \"value\" /* inline */\n}\n",
        "config.jsonc",
        Language::Jsonc,
    );
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "JSONC with valid comments should have no errors; got: {diags:?}"
    );
}

/// Behavioral proof: a short JSON array stays inline after formatting.
///
/// serde_json's `to_string_pretty` used to explode `["CodeBlock","Code"]` to
/// one element per line; oxc_formatter_json packs them inline when they fit.
#[test]
fn oxc_format_json_short_array_stays_inline() {
    let src = make_src(r#"{"parsers":["CodeBlock","Code"]}"#, "config.json", Language::Json);
    let out = OxcEngine.format(&src, &default_cfg()).unwrap();
    let formatted = match out {
        poly_core::engine::FormatOutput::Formatted(text) => text,
        poly_core::engine::FormatOutput::Unchanged => {
            panic!("expected Formatted, got Unchanged");
        }
    };
    assert!(
        formatted.contains(r#"["CodeBlock", "Code"]"#),
        "short array should stay inline; got:\n{formatted}"
    );
}

/// Behavioral proof: JSONC comments are preserved through `format()`.
///
/// Previously format_json returned FormatOutput::Unchanged for JSONC; now it
/// reformats via oxc_formatter_json which must re-emit the comments.
#[test]
fn oxc_format_jsonc_preserves_comments() {
    let src = make_src(
        "{\n  // line comment\n  \"key\": \"value\" /* block comment */\n}\n",
        "config.jsonc",
        Language::Jsonc,
    );
    let out = OxcEngine.format(&src, &default_cfg()).unwrap();
    let text = match out {
        poly_core::engine::FormatOutput::Formatted(text) => text,
        poly_core::engine::FormatOutput::Unchanged => src.content.to_string(),
    };
    assert!(
        text.contains("// line comment"),
        "line comment must survive JSONC formatting; got:\n{text}"
    );
    assert!(
        text.contains("/* block comment */"),
        "block comment must survive JSONC formatting; got:\n{text}"
    );
}

// ── Opinionated default rule set (Phase 3.0) ─────────────────────────────────
//
// `ConfigStoreBuilder::default()` upstream means the `correctness` category
// only. These assert the widened default — `suspicious` + `pedantic` plus three
// named `restriction` rules — actually fires with no engine config, and that the
// three rules tuned back out stay silent. Each was verified to fail against the
// previous correctness-only default.

/// Returns every rule code the default (no-config) path reports for `content`.
fn default_codes(content: &str, path: &str, lang: Language) -> Vec<String> {
    OxcEngine
        .lint(&make_src(content, path, lang), &default_cfg())
        .unwrap()
        .into_iter()
        .filter_map(|d| d.code)
        .collect()
}

/// The `restriction` rules named individually in `DEFAULT_LINT_FILTERS`:
/// `any`, the non-null assertion `!`, and a shipped `console` call.
#[test]
fn oxc_named_restriction_rules_fire_by_default() {
    let any_codes = default_codes(
        "export function f(x: any): any {\n  return x;\n}\n",
        "a.ts",
        Language::TypeScript,
    );
    assert!(
        any_codes.iter().any(|c| c == "typescript/no-explicit-any"),
        "`: any` must trip typescript/no-explicit-any; got: {any_codes:?}"
    );

    let bang_codes = default_codes(
        "export function f(x: string | null): number {\n  return x!.length;\n}\n",
        "b.ts",
        Language::TypeScript,
    );
    assert!(
        bang_codes.iter().any(|c| c == "typescript/no-non-null-assertion"),
        "`!.` must trip typescript/no-non-null-assertion; got: {bang_codes:?}"
    );

    let console_codes = default_codes(
        "export function f() {\n  console.log('hi');\n}\n",
        "c.js",
        Language::JavaScript,
    );
    assert!(
        console_codes.iter().any(|c| c == "no-console"),
        "console.log must trip no-console; got: {console_codes:?}"
    );
}

/// The `suspicious` and `pedantic` categories: `==` instead of `===` is the
/// canonical member of the gap the correctness-only default left open.
#[test]
fn oxc_suspicious_and_pedantic_categories_fire_by_default() {
    let codes = default_codes(
        "export function f(a: number, b: string): boolean {\n  return a == b;\n}\n",
        "d.ts",
        Language::TypeScript,
    );
    assert!(
        codes.iter().any(|c| c == "eqeqeq"),
        "`==` must trip eqeqeq once suspicious/pedantic are on; got: {codes:?}"
    );
}

/// `no-underscore-dangle` is in `DEFAULT_ALLOWED_RULES` — it rides along with
/// `pedantic` but was measured at 3.0 findings per file on the corpus, almost
/// all of them the deliberate `_private` convention. Off by default, back with
/// `extend_select`.
#[test]
fn oxc_tuned_out_rule_is_off_by_default_but_reenableable() {
    let content = "export function f() {\n  const _cached = 1;\n  return _cached;\n}\n";

    let codes = default_codes(content, "e.js", Language::JavaScript);
    assert!(
        !codes.iter().any(|c| c == "no-underscore-dangle"),
        "no-underscore-dangle must be off by default; got: {codes:?}"
    );

    let mut opts = toml::Table::new();
    opts.insert(
        "extend_select".to_owned(),
        toml::Value::Array(vec![toml::Value::String("no-underscore-dangle".to_owned())]),
    );
    let reenabled = OxcEngine
        .lint(
            &make_src(content, "e.js", Language::JavaScript),
            &EngineConfig {
                globals: GlobalDefaults::default(),
                indent_width: 2,
                options: opts,
            },
        )
        .unwrap();
    assert!(
        reenabled
            .iter()
            .any(|d| d.code.as_deref() == Some("no-underscore-dangle")),
        "extend_select must bring no-underscore-dangle back; got: {reenabled:?}"
    );
}

/// The widened default and the user-config path share one filter list, so a
/// rule that fires with no config must still fire when an unrelated config key
/// is present — the drift guard on `DEFAULT_LINT_FILTERS`.
#[test]
fn oxc_widened_default_survives_unrelated_user_config() {
    let content = "export function f(x: any): any {\n  return x;\n}\n";

    let mut opts = toml::Table::new();
    opts.insert(
        "ignore".to_owned(),
        toml::Value::Array(vec![toml::Value::String("no-debugger".to_owned())]),
    );
    let diags = OxcEngine
        .lint(
            &make_src(content, "f.ts", Language::TypeScript),
            &EngineConfig {
                globals: GlobalDefaults::default(),
                indent_width: 2,
                options: opts,
            },
        )
        .unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d.code.as_deref() == Some("typescript/no-explicit-any")),
        "the opinionated base must apply on the user-config path too; got: {diags:?}"
    );
}
