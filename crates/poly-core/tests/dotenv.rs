//! Insta snapshot fixtures for the dotenv linter backend.
//!
//! - `known_bad_dotenv_diagnostics` — a `.env` file with several deliberate
//!   violations (duplicated key, unordered key, lowercase key, an unquoted
//!   value with spaces, trailing whitespace) asserts the expected
//!   [`Diagnostic`]s.
//! - `known_bad_dotenv_fix_applies_cleanly` — applies the fix [`Edit`] carried
//!   by the first diagnostic and asserts the exact corrected text.
//! - `clean_file_has_no_dotenv_diagnostics` — a properly-ordered, uppercase,
//!   unique-key `.env` file produces no findings.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::dotenv::DotenvEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: ".env".into(),
        language: Language::Dotenv,
        content: content.into(),
    }
}

const KNOWN_BAD: &str = include_str!("fixtures/dotenv/known_bad.env");
const CLEAN: &str = include_str!("fixtures/dotenv/clean.env");

#[test]
fn known_bad_dotenv_diagnostics() {
    let engine = DotenvEngine;
    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected dotenv diagnostics for the known-bad file");

    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.title.as_str(),
                d.span.as_ref().map(|s| (s.start_line, s.start_col)),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_dotenv_diagnostics", summary);
}

#[test]
fn known_bad_dotenv_fix_applies_cleanly() {
    let engine = DotenvEngine;
    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    let edit = diags
        .iter()
        .find_map(|d| d.fix.first())
        .expect("expected the first diagnostic to carry the combined fix edit");

    let mut corrected = KNOWN_BAD.to_string();
    corrected.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);

    insta::assert_snapshot!("known_bad_dotenv_fixed", corrected);
}

#[test]
fn clean_file_has_no_dotenv_diagnostics() {
    let engine = DotenvEngine;
    let src = make_src(CLEAN);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for a clean file, got: {diags:?}"
    );
}
