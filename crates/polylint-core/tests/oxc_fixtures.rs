//! insta snapshot fixtures for the oxc backend.
//! Two kinds:
//!   1. known-bad file  → expected `Diagnostic`s
//!   2. known-unformatted file → exact formatted output

use std::path::PathBuf;

use polylint_core::config::{EngineConfig, GlobalDefaults};
use polylint_core::engine::{Engine, SourceFile};
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

/// Syntactically broken JS file — asserts the expected parse-error Diagnostic.
#[test]
fn oxc_known_bad_js_diagnostics() {
    let src = make_src(
        "const x = {\n  a: 1,\nconst y = 2;\n",
        "bad.js",
        Language::JavaScript,
    );
    let diags = OxcEngine.lint(&src, &default_cfg()).unwrap();
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    insta::assert_debug_snapshot!(diags);
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

/// Compact JS file → asserts exact formatted output from oxc_codegen.
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
