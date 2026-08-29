//! Insta snapshot fixtures for the INI linter backend.
//!
//! - `known_bad_ini_diagnostics` — an `.ini` file with a duplicated key, a
//!   duplicated section, a bare key with no value, an inconsistent separator,
//!   trailing whitespace, and a malformed (unclosed) section header asserts
//!   the expected [`Diagnostic`]s.
//! - `parse_error_uses_real_parser_line_and_col` — a minimal syntax error
//!   (`=value` with no key) asserts the `parse-error` rule surfaces
//!   `rust-ini`'s own line/column, as an `Error`-severity finding.
//! - `npmrc_shaped_fixture_has_no_diagnostics` — a flat, section-less
//!   `.npmrc`-style file (the case most likely to be implemented wrong: a
//!   `//host:_authToken=...` key contains a `:` *before* its real `=`
//!   separator) produces no findings.
//! - `clean_file_has_no_ini_diagnostics` — a well-formed `.ini` file produces
//!   no findings.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, Severity, SourceFile},
    engines::ini::IniEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Ini,
        content: content.into(),
    }
}

const KNOWN_BAD: &str = include_str!("fixtures/ini/known_bad.ini");
const CLEAN: &str = include_str!("fixtures/ini/clean.ini");
const NPMRC_SHAPED: &str = include_str!("fixtures/ini/npmrc_shaped.npmrc");

#[test]
fn known_bad_ini_diagnostics() {
    let engine = IniEngine;
    let src = make_src("known_bad.ini", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected ini diagnostics for the known-bad file");

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
    insta::assert_debug_snapshot!("known_bad_ini_diagnostics", summary);
}

#[test]
fn parse_error_uses_real_parser_line_and_col() {
    let engine = IniEngine;
    let src = make_src("bad_parse.ini", "[section]\n=missingkey\n");
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    let parse_diag = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("parse-error"))
        .unwrap_or_else(|| panic!("expected a parse-error diagnostic, got: {diags:?}"));
    assert_eq!(parse_diag.severity, Severity::Error);
    let span = parse_diag.span.expect("parse-error must carry a span");
    assert_eq!(span.start_line, 2, "the syntax error is on line 2 (`=missingkey`)");
}

#[test]
fn npmrc_shaped_fixture_has_no_diagnostics() {
    let engine = IniEngine;
    let src = make_src(".npmrc", NPMRC_SHAPED);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "a flat .npmrc-shaped file must not false-positive: {diags:?}"
    );
}

#[test]
fn clean_file_has_no_ini_diagnostics() {
    let engine = IniEngine;
    let src = make_src("clean.ini", CLEAN);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for a clean file, got: {diags:?}"
    );
}
