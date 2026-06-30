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
        content: content.into(),
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

// ---------------------------------------------------------------------------
// Config-driven tests: rule selection, ignore, level override, format options.
//
// Test file: `<?php\nvar_dump('hello');\n`
//   Fires (default config):
//     - strict-types   (Correctness, Warning) — no declare(strict_types=1)
//     - no-debug-symbols (Security, Info)     — var_dump usage
// ---------------------------------------------------------------------------

/// A PHP file that triggers rules from two different categories.
///
/// - `strict-types`      — Correctness — fired by absence of `declare(strict_types=1)`
/// - `no-debug-symbols`  — Security    — fired by `var_dump` call
const MULTI_CATEGORY_PHP: &str = "<?php\nvar_dump('hello');\n";

fn cfg_from_str(toml_str: &str) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::from_str(toml_str).expect("valid TOML"),
    }
}

/// Default config fires both `strict-types` (Correctness) and
/// `no-debug-symbols` (Security).
#[test]
fn default_config_fires_both_categories() {
    let engine = MagoEngine;
    let src = make_src("multi.php", MULTI_CATEGORY_PHP);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();

    assert!(
        codes.contains(&"strict-types"),
        "expected strict-types in default results; got: {codes:?}"
    );
    assert!(
        codes.contains(&"no-debug-symbols"),
        "expected no-debug-symbols in default results; got: {codes:?}"
    );
}

/// `select = ["correctness"]` limits results to Correctness rules only.
/// - `strict-types` (Correctness) must still appear.
/// - `no-debug-symbols` (Security) must be absent.
#[test]
fn select_by_category_restricts_to_correctness_rules() {
    let engine = MagoEngine;
    let src = make_src("multi.php", MULTI_CATEGORY_PHP);
    let cfg = cfg_from_str(r#"select = ["correctness"]"#);

    let diags = engine.lint(&src, &cfg).unwrap();
    let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();

    assert!(
        codes.contains(&"strict-types"),
        "strict-types (correctness) should fire; got: {codes:?}"
    );
    assert!(
        !codes.contains(&"no-debug-symbols"),
        "no-debug-symbols (security) should be absent; got: {codes:?}"
    );
}

/// `ignore = ["strict-types"]` suppresses exactly that rule.
/// `no-debug-symbols` must still appear.
#[test]
fn ignore_code_suppresses_that_finding() {
    let engine = MagoEngine;
    let src = make_src("multi.php", MULTI_CATEGORY_PHP);
    let cfg = cfg_from_str(r#"ignore = ["strict-types"]"#);

    let diags = engine.lint(&src, &cfg).unwrap();
    let codes: Vec<_> = diags.iter().filter_map(|d| d.code.as_deref()).collect();

    assert!(
        !codes.contains(&"strict-types"),
        "strict-types should be suppressed; got: {codes:?}"
    );
    assert!(
        codes.contains(&"no-debug-symbols"),
        "no-debug-symbols should still fire; got: {codes:?}"
    );
}

/// `[rules.strict-types] level = "error"` overrides that rule's severity from
/// Warning to Error.
#[test]
fn level_override_changes_severity_to_error() {
    let engine = MagoEngine;
    let src = make_src("multi.php", MULTI_CATEGORY_PHP);
    let cfg = cfg_from_str(
        r#"
[rules.strict-types]
level = "error"
"#,
    );

    let diags = engine.lint(&src, &cfg).unwrap();
    let strict_types_diag = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("strict-types"))
        .expect("strict-types should fire");

    assert_eq!(
        strict_types_diag.severity,
        polylint_core::engine::Severity::Error,
        "strict-types level override to 'error' should produce Severity::Error"
    );
}

// ---------------------------------------------------------------------------
// Format option fixture: `function-brace-style = "same_line"` changes output.
//
// mago default: function braces on next line  `function foo()\n{\n`
// With same_line:                              `function foo() {\n`
// ---------------------------------------------------------------------------

/// A PHP function whose brace placement we can verify.
const FUNCTION_PHP: &str = "<?php\nfunction foo() {\n    return 1;\n}\n";

/// Default `function-brace-style` is `next_line`; the formatter moves the
/// opening brace to a new line.
#[test]
fn default_format_places_function_brace_on_next_line() {
    let engine = MagoEngine;
    let src = make_src("fn.php", FUNCTION_PHP);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            assert!(
                text.contains("function foo()\n{"),
                "expected next-line brace style; got:\n{text}"
            );
        }
        FormatOutput::Unchanged => panic!("expected Formatted"),
    }
}

/// `function-brace-style = "same_line"` keeps the brace on the same line as
/// the function signature.  Verified by snapshot.
#[test]
fn format_option_function_brace_style_same_line() {
    let engine = MagoEngine;
    let src = make_src("fn.php", FUNCTION_PHP);
    let cfg = cfg_from_str(r#"function-brace-style = "same_line""#);
    let formatted = match engine.format(&src, &cfg).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => panic!("expected Formatted"),
    };
    // The opening brace must be on the same line as the function signature.
    assert!(
        formatted.contains("function foo() {"),
        "expected same-line brace style; got:\n{formatted}"
    );
    // Snapshot the exact output for regression tracking.
    insta::assert_snapshot!("mago_format_same_line_brace", formatted);
}
