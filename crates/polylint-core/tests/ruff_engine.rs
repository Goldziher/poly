//! Insta snapshot fixtures for the ruff Python backend.
//!
//! - `known_bad_diagnostics` — a Python file with real rule violations asserts
//!   the expected [`Diagnostic`]s (F401, W605, E711).
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
        content: content.into(),
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
// Known-bad fixture: real rule violations produce expected diagnostics.
//
// Violations in this snippet:
//   F401 — `os` is imported but never used
//   W605 — `"\s"` contains an invalid escape sequence
//   E711 — comparison to None should use `is`/`is not`
//
// Note: avoid trailing whitespace and misspellings — pre-commit hooks rewrite
// them and would break the test constants.
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = "\
import os
x = \"\\s\"
def check(val):
    if val == None:
        pass
";

#[test]
fn known_bad_diagnostics() {
    let engine = RuffEngine;
    let src = make_src("known_bad.py", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected rule diagnostics");

    // Assert structural properties: engine name, non-empty code, line presence.
    for diag in &diags {
        assert_eq!(diag.engine, "ruff");
        assert!(
            diag.code.is_some(),
            "every ruff diagnostic must carry a rule code"
        );
        assert!(
            diag.span.is_some(),
            "every ruff diagnostic must carry a span"
        );
    }

    // Collect (code, start_line) for snapshot.
    let mut summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.code.as_deref().unwrap_or(""),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();
    // Sort for determinism — ruff does not guarantee order across rules.
    summary.sort_unstable();

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

/// Regression: ruff's INP001 (implicit-namespace-package) must respect the
/// real on-disk package root. A module inside a package (with `__init__.py`)
/// must NOT be flagged, even though poly lints one file at a time — the engine
/// resolves the package root from disk. A module in a dir with no `__init__.py`
/// still trips it (sanity that the rule is active in this config).
#[test]
fn inp001_respects_on_disk_package_root() {
    use std::fs;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("pkg")).unwrap();
    fs::write(root.join("pkg/__init__.py"), "").unwrap();
    fs::write(root.join("pkg/mod.py"), "x = 1\n").unwrap();
    fs::create_dir_all(root.join("loose")).unwrap();
    fs::write(root.join("loose/orphan.py"), "x = 1\n").unwrap();

    let engine = RuffEngine;
    let mut options = toml::Table::new();
    options.insert(
        "select".to_string(),
        toml::Value::Array(vec![toml::Value::String("INP".into())]),
    );
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };

    let in_pkg = SourceFile {
        path: root.join("pkg/mod.py"),
        language: Language::Python,
        content: "x = 1\n".into(),
    };
    let pkg = engine.lint(&in_pkg, &cfg).unwrap();
    assert!(
        !pkg.iter().any(|d| d.code.as_deref() == Some("INP001")),
        "a module in a package (has __init__.py) must not trip INP001; got: {pkg:?}"
    );

    let orphan = SourceFile {
        path: root.join("loose/orphan.py"),
        language: Language::Python,
        content: "x = 1\n".into(),
    };
    let orphan_diags = engine.lint(&orphan, &cfg).unwrap();
    assert!(
        orphan_diags
            .iter()
            .any(|d| d.code.as_deref() == Some("INP001")),
        "a module with no __init__.py must trip INP001 (rule is active); got: {orphan_diags:?}"
    );
}

/// `mccabe_max_complexity` is honored: C901 fires at a low threshold, not a high one.
#[test]
fn mccabe_max_complexity_param_is_honored() {
    let engine = RuffEngine;
    let src = make_src(
        "m.py",
        "def f(x):\n    if x == 1:\n        return 1\n    elif x == 2:\n        return 2\n    elif x == 3:\n        return 3\n    elif x == 4:\n        return 4\n    return 0\n",
    );
    let cfg = |max: i64| {
        let mut o = toml::Table::new();
        o.insert(
            "select".to_string(),
            toml::Value::Array(vec![toml::Value::String("C901".into())]),
        );
        o.insert(
            "mccabe_max_complexity".to_string(),
            toml::Value::Integer(max),
        );
        EngineConfig {
            options: o,
            ..engine_cfg()
        }
    };
    let fired = |c: &EngineConfig| {
        engine
            .lint(&make_src("m.py", &src.content), c)
            .unwrap()
            .iter()
            .any(|d| d.code.as_deref() == Some("C901"))
    };
    assert!(fired(&cfg(1)), "C901 must fire at max_complexity=1");
    assert!(!fired(&cfg(50)), "C901 must not fire at max_complexity=50");
}

/// `pydocstyle_convention = "google"` disables the D-rules google turns off,
/// so a docstring trips fewer D findings than with no convention.
#[test]
fn pydocstyle_convention_reduces_d_rules() {
    let engine = RuffEngine;
    let body = "def f():\n    \"\"\"Summary.\n\n    Body.\n    \"\"\"\n    return 1\n";
    let count_d = |opts: toml::Table| {
        engine
            .lint(
                &make_src("m.py", body),
                &EngineConfig {
                    options: opts,
                    ..engine_cfg()
                },
            )
            .unwrap()
            .iter()
            .filter(|d| d.code.as_deref().is_some_and(|c| c.starts_with('D')))
            .count()
    };
    let mut base = toml::Table::new();
    base.insert(
        "select".to_string(),
        toml::Value::Array(vec![toml::Value::String("D".into())]),
    );
    let no_conv = count_d(base.clone());
    base.insert(
        "pydocstyle_convention".to_string(),
        toml::Value::String("google".into()),
    );
    let google = count_d(base);
    assert!(
        google < no_conv,
        "google convention should disable some D-rules: no_conv={no_conv} google={google}"
    );
}
