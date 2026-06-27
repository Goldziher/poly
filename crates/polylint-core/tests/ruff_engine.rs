//! Insta snapshot fixtures for the ruff Python backend.
//!
//! - `known_bad_diagnostics` — a Python file with a syntax error asserts the
//!   expected parse-error [`Diagnostic`] (`E999`).
//! - `known_unformatted_output` — a badly-formatted Python file asserts the
//!   exact output produced by the ruff formatter.
//! - `docstring_code_format_output` — proves the opinionated
//!   `docstring-code-format` default reformats code blocks inside docstrings.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::ruff::RuffEngine,
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
        language: Language::Python,
        content: content.to_string(),
    }
}

fn format_to_string(content: &str) -> String {
    let engine = RuffEngine;
    let src = make_src("fixture.py", content);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: a syntax error produces an E999 parse diagnostic.
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = "\
def broken(:
    return 1
";

#[test]
fn known_bad_diagnostics() {
    let engine = RuffEngine;
    let src = make_src("known_bad.py", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected a parse-error diagnostic");
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

#[test]
fn valid_python_has_no_diagnostics() {
    let engine = RuffEngine;
    let src = make_src("ok.py", "def ok():\n    return 1\n");
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(diags.is_empty(), "got: {diags:?}");
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: messy spacing/quotes → ruff-formatted output.
// ---------------------------------------------------------------------------

const KNOWN_UNFORMATTED: &str = "\
def  add(a,b ):
  x = {'a':1,'b':2}
  return a+b
";

#[test]
fn known_unformatted_output() {
    insta::assert_snapshot!(
        "known_unformatted_output",
        format_to_string(KNOWN_UNFORMATTED)
    );
}

// ---------------------------------------------------------------------------
// Docstring code formatting: the opinionated `docstring-code-format` default
// reformats Python code blocks embedded in docstrings.
// ---------------------------------------------------------------------------

const DOCSTRING_CODE: &str = "\
def example():
    \"\"\"Summary.

    >>> x=1
    >>> y=[1,2,3]
    \"\"\"
    return None
";

#[test]
fn docstring_code_format_output() {
    insta::assert_snapshot!(
        "docstring_code_format_output",
        format_to_string(DOCSTRING_CODE)
    );
}
