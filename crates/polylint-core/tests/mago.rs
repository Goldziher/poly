//! Insta snapshot fixtures for the PHP/mago backend.
//!
//! - `known_bad_diagnostics` — a PHP file with a syntax error and a lint issue
//!   asserts the expected [`Diagnostic`]s structurally (engine, code, severity).
//! - `known_unformatted_output` — a PHP file with dense, unstyled formatting
//!   asserts the exact formatted output produced by mago.
//! - `valid_php_has_no_parse_errors` — a clean PHP file returns no parse-error
//!   diagnostics.
//! - `already_formatted_returns_unchanged` — a well-formatted PHP file is
//!   returned as [`FormatOutput::Unchanged`].

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::mago::MagoEngine,
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
        language: Language::Php,
        content: content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: unclosed class body triggers a parse error.
// ---------------------------------------------------------------------------

/// Unclosed class — the parser expects `}` before EOF.
const KNOWN_BAD: &str = "<?php\nclass Foo {\n    public function bar(\n";

#[test]
fn known_bad_diagnostics() {
    let engine = MagoEngine;
    let src = make_src("known_bad.php", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    // Must have at least one parse-error diagnostic.
    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic, got none"
    );

    // Structural snapshot: (engine, code, severity) — not brittle on the exact
    // message text which may vary between mago versions.
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.severity,
            )
        })
        .collect();
    insta::assert_debug_snapshot!("mago_known_bad_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Valid PHP: a syntactically clean file produces no parse-error diagnostics.
// ---------------------------------------------------------------------------

/// A minimal valid PHP 8.4 file.
const VALID_PHP: &str = "<?php\n\ndeclare(strict_types=1);\n\nfinal class Calculator\n{\n    public function add(int $a, int $b): int\n    {\n        return $a + $b;\n    }\n}\n";

#[test]
fn valid_php_has_no_parse_errors() {
    let engine = MagoEngine;
    let src = make_src("valid.php", VALID_PHP);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    let parse_errors: Vec<_> = diags
        .iter()
        .filter(|d| d.code.as_deref() == Some("syntax") || d.code.as_deref() == Some("parse"))
        .collect();
    assert!(
        parse_errors.is_empty(),
        "expected no parse errors for valid PHP, got: {parse_errors:?}"
    );
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: dense PHP gets reformatted to PER-CS style.
// ---------------------------------------------------------------------------

/// Dense single-line class with missing spaces — mago should expand it.
const KNOWN_UNFORMATTED: &str = "<?php\nclass Foo{public function bar(){return 1+2;}}\n";

#[test]
fn known_unformatted_output() {
    let engine = MagoEngine;
    let src = make_src("unformatted.php", KNOWN_UNFORMATTED);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            insta::assert_snapshot!("mago_known_unformatted_output", text);
        }
        FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
    }
}

// ---------------------------------------------------------------------------
// Already-formatted fixture: a well-formatted PHP file stays Unchanged.
// ---------------------------------------------------------------------------

#[test]
fn already_formatted_returns_unchanged() {
    let engine = MagoEngine;
    // Use a copy of what mago produces for KNOWN_UNFORMATTED; the exact
    // content is checked in `known_unformatted_output`.  Here we just verify
    // the Unchanged contract on already-clean input.
    let src = make_src("clean.php", VALID_PHP);
    let result = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(result, FormatOutput::Unchanged),
        "expected Unchanged for already-clean PHP"
    );
}
