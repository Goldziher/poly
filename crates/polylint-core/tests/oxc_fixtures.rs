//! insta snapshot fixtures for the oxc backend.
//! Two kinds:
//!   1. known-bad file  → expected `Diagnostic`s
//!   2. known-unformatted file → exact formatted output

use std::path::PathBuf;

use polylint_core::config::{EngineConfig, GlobalDefaults};
use polylint_core::engine::{Engine, Severity, SourceFile};
use polylint_core::engines::oxc::OxcEngine;
use polylint_core::language::Language;

fn make_src(content: &str, path: &str, lang: Language) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language: lang,
        content: content.to_owned(),
    }
}

fn default_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

// ── known-bad fixtures ────────────────────────────────────────────────────────

/// Syntactically broken JS file — asserts that at least one Error-severity
/// diagnostic is produced. The exact message format is not snapshotted here
/// because it comes from oxlint's internal parser and may evolve.
#[test]
fn oxc_known_bad_js_diagnostics() {
    let src = make_src(
        "const x = {\n  a: 1,\nconst y = 2;\n",
        "bad.js",
        Language::JavaScript,
    );
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    // oxlint reports parse errors at Error severity with no rule code.
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

    // Structural assertions: at least one no-debugger warning must appear.
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

    // Snapshot: count + (code, severity) pairs for structural verification.
    let summary: Vec<(Option<&str>, &Severity)> = diags
        .iter()
        .map(|d| (d.code.as_deref(), &d.severity))
        .collect();
    insta::assert_debug_snapshot!(summary);
}

/// JSON with a trailing comma — asserts the expected parse-error Diagnostic.
#[test]
fn oxc_known_bad_json_diagnostics() {
    let src = make_src(
        "{\n  \"a\": 1,\n  \"b\": 2,\n}\n",
        "bad.json",
        Language::Json,
    );
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic for trailing comma"
    );
    insta::assert_debug_snapshot!(diags[0].message);
}

// ── known-unformatted fixtures ────────────────────────────────────────────────

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
        polylint_core::engine::FormatOutput::Formatted(text) => {
            insta::assert_snapshot!(text);
        }
        polylint_core::engine::FormatOutput::Unchanged => {
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
        polylint_core::engine::FormatOutput::Formatted(text) => {
            insta::assert_snapshot!(text);
        }
        polylint_core::engine::FormatOutput::Unchanged => {
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
