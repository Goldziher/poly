//! Insta snapshot fixtures for the rubyfmt Ruby backend.
//!
//! - `should_reformat_misindented_ruby` — a Ruby method with loose argument
//!   spacing and bare `puts` calls asserts the exact formatted output produced
//!   by rubyfmt (parens normalized, spacing tightened).
//! - `should_return_unchanged_on_unparsable_ruby` — an unclosed `def`
//!   asserts `FormatOutput::Unchanged` (robustness rule: parse errors must
//!   not crash the run or corrupt the file).

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::rubyfmt::RubyfmtEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Ruby,
        content: content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: loose argument spacing + bare puts call.
//
// rubyfmt normalises `greet( name )` → `greet(name)` (no spaces inside
// parens) and `puts "hello"` → `puts("hello")` (adds required parens).
// The blank line inside the `if` body is preserved — only leading/trailing
// blank lines inside block delimiters are removed.
// ---------------------------------------------------------------------------

/// Unformatted Ruby: spaces inside method-param parens and bare `puts`.
///
/// No trailing whitespace on any line so the `trailing-whitespace` prek
/// hook does not rewrite this literal.
const KNOWN_UNFORMATTED: &str =
    "def greet( name )\n  x = 1 + 2\n  if x > 3\n\n    puts \"hello\"\n  end\nend\n";

#[test]
fn should_reformat_misindented_ruby() {
    let engine = RubyfmtEngine;
    let src = make_src("greet.rb", KNOWN_UNFORMATTED);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => {
            insta::assert_snapshot!("rubyfmt_known_unformatted_output", text);
        }
        FormatOutput::Unchanged => panic!("expected Formatted, got Unchanged"),
    }
}

// ---------------------------------------------------------------------------
// Robustness fixture: syntax error must not crash or propagate an error.
// ---------------------------------------------------------------------------

#[test]
fn should_return_unchanged_on_unparsable_ruby() {
    let engine = RubyfmtEngine;
    // An unclosed `def foo(` is a syntax error; rubyfmt returns SyntaxError,
    // which the engine maps to Unchanged (robustness rule).
    let src = make_src("bad.rb", "def foo(");
    let out = engine.format(&src, &engine_cfg()).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "expected Unchanged for syntax-error input, got Formatted"
    );
}
