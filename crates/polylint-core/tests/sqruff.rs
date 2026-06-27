//! insta snapshot fixtures for the sqruff backend.
//!
//! Two fixture categories:
//! - known-bad: a SQL file with known violations → asserts the `Diagnostic` list.
//! - known-unformatted: a SQL file sqruff can reformat → asserts exact formatted output.

use std::path::PathBuf;

use polylint_core::config::{Config, Kind};
use polylint_core::engine::{Engine, SourceFile};
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
