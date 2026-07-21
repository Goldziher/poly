//! Insta snapshot fixtures for the ruff Python backend.
//!
//! - `known_bad_diagnostics` — a Python file with real rule violations asserts
//!   the expected [`Diagnostic`]s (F401, W605, E711).
//! - `known_unformatted_output` — a badly-formatted Python file asserts the
//!   exact output produced by the ruff formatter.
//! - `docstring_code_format_output` — proves the opinionated
//!   `docstring-code-format` default reformats code blocks inside docstrings.

use poly_core::{
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

/// Build a TOML string-array value from a slice of codes.
fn code_array(codes: &[&str]) -> toml::Value {
    toml::Value::Array(codes.iter().map(|c| toml::Value::String((*c).into())).collect())
}

fn format_to_string(content: &str) -> String {
    let engine = RuffEngine;
    let src = make_src("fixture.py", content);
    match engine.format(&src, &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => content.to_string(),
    }
}

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

    for diag in &diags {
        assert_eq!(diag.engine, "ruff");
        assert!(diag.code.is_some(), "every ruff diagnostic must carry a rule code");
        assert!(diag.span.is_some(), "every ruff diagnostic must carry a span");
    }

    let mut summary: Vec<_> = diags
        .iter()
        .map(|d| (d.code.as_deref().unwrap_or(""), d.span.as_ref().map(|s| s.start_line)))
        .collect();
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

const KNOWN_UNFORMATTED: &str = "\
def  add(a,b ):
  x = {'a':1,'b':2}
  return a+b
";

#[test]
fn known_unformatted_output() {
    insta::assert_snapshot!("known_unformatted_output", format_to_string(KNOWN_UNFORMATTED));
}

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
    insta::assert_snapshot!("docstring_code_format_output", format_to_string(DOCSTRING_CODE));
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
        orphan_diags.iter().any(|d| d.code.as_deref() == Some("INP001")),
        "a module with no __init__.py must trip INP001 (rule is active); got: {orphan_diags:?}"
    );
}

/// Canonical `extend_select` adds a rule on top of the default set: `print(1)`
/// is clean under the defaults (F/E4/E7/E9/W6/I/UP/B) but trips T201
/// (flake8-print) once `extend_select = ["T201"]` is applied.
#[test]
fn extend_select_adds_rule_beyond_defaults() {
    let engine = RuffEngine;
    let content = "print(1)\n";

    let base = engine.lint(&make_src("m.py", content), &engine_cfg()).unwrap();
    assert!(
        !base.iter().any(|d| d.code.as_deref() == Some("T201")),
        "T201 must not fire under the default rule set; got: {base:?}"
    );

    let mut options = toml::Table::new();
    options.insert("extend_select".to_string(), code_array(&["T201"]));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let extended = engine.lint(&make_src("m.py", content), &cfg).unwrap();
    assert!(
        extended.iter().any(|d| d.code.as_deref() == Some("T201")),
        "extend_select = [\"T201\"] must flag print; got: {extended:?}"
    );
}

/// B008 (function-call in argument default) is disabled by default so the
/// idiomatic FastAPI/typer `x = Depends(...)` pattern is not flagged, while the
/// rest of flake8-bugbear stays on. `extend_select = ["B008"]` brings it back.
#[test]
fn b008_is_off_by_default_but_reenableable() {
    let engine = RuffEngine;
    // `Depends()` in a default triggers B008; the mutable-default arg triggers B006.
    let content = "def f(x=Depends()):\n    return x\n\n\ndef g(items=[]):\n    return items\n";

    let default = engine.lint(&make_src("m.py", content), &engine_cfg()).unwrap();
    assert!(
        !default.iter().any(|d| d.code.as_deref() == Some("B008")),
        "B008 must be off by default; got: {default:?}"
    );
    assert!(
        default.iter().any(|d| d.code.as_deref() == Some("B006")),
        "the rest of bugbear (B006) must still fire; got: {default:?}"
    );

    let mut options = toml::Table::new();
    options.insert("extend_select".to_string(), code_array(&["B008"]));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let reenabled = engine.lint(&make_src("m.py", content), &cfg).unwrap();
    assert!(
        reenabled.iter().any(|d| d.code.as_deref() == Some("B008")),
        "extend_select = [\"B008\"] must re-enable it; got: {reenabled:?}"
    );
}

/// Regression: canonical `select` narrows the active set and `ignore` removes a
/// rule from it. `select = ["F"]` flags F401 (unused import); adding
/// `ignore = ["F401"]` suppresses it.
#[test]
fn canonical_select_and_ignore_are_honored() {
    let engine = RuffEngine;

    let mut select_only = toml::Table::new();
    select_only.insert("select".to_string(), code_array(&["F"]));
    let selected = engine
        .lint(
            &make_src("known_bad.py", KNOWN_BAD),
            &EngineConfig {
                options: select_only,
                ..engine_cfg()
            },
        )
        .unwrap();
    assert!(
        selected.iter().any(|d| d.code.as_deref() == Some("F401")),
        "select = [\"F\"] must flag F401; got: {selected:?}"
    );

    let mut with_ignore = toml::Table::new();
    with_ignore.insert("select".to_string(), code_array(&["F"]));
    with_ignore.insert("ignore".to_string(), code_array(&["F401"]));
    let ignored = engine
        .lint(
            &make_src("known_bad.py", KNOWN_BAD),
            &EngineConfig {
                options: with_ignore,
                ..engine_cfg()
            },
        )
        .unwrap();
    assert!(
        !ignored.iter().any(|d| d.code.as_deref() == Some("F401")),
        "ignore = [\"F401\"] must suppress F401; got: {ignored:?}"
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
        o.insert("mccabe_max_complexity".to_string(), toml::Value::Integer(max));
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

/// `known_first_party` suppresses I001 for a module that would otherwise be
/// classified as third-party. Without `known_first_party`, importing
/// `kreuzberg_cloud` after `pytest` (also third-party) triggers I001 because
/// isort expects alphabetical order within the third-party block (`kreuzberg_cloud`
/// before `pytest`), but the file has `pytest` first. With
/// `known_first_party = ["kreuzberg_cloud"]`, the module is first-party and
/// correctly placed after `pytest` — no I001.
#[test]
fn known_first_party_suppresses_i001() {
    use std::fs;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_module.py");
    fs::write(&path, "").unwrap();

    let body = "import os\n\nimport pytest\n\nimport kreuzberg_cloud\n";

    let engine = RuffEngine;

    let src = SourceFile {
        path: path.clone(),
        language: Language::Python,
        content: body.into(),
    };

    let base_diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        base_diags.iter().any(|d| d.code.as_deref() == Some("I001")),
        "without known_first_party, I001 must fire (kreuzberg_cloud is third-party, out of alpha order); got: {base_diags:?}"
    );

    let mut options = toml::Table::new();
    options.insert(
        "known_first_party".to_string(),
        toml::Value::Array(vec![toml::Value::String("kreuzberg_cloud".into())]),
    );
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let src2 = SourceFile {
        path,
        language: Language::Python,
        content: body.into(),
    };
    let fp_diags = engine.lint(&src2, &cfg).unwrap();
    assert!(
        !fp_diags.iter().any(|d| d.code.as_deref() == Some("I001")),
        "with known_first_party=[\"kreuzberg_cloud\"], I001 must not fire; got: {fp_diags:?}"
    );
}

/// Regression: E501 (line-too-long) must honor the configured `line_length`
/// instead of ruff's hardcoded pycodestyle default of 88. A 100-char line is
/// clean at `line_length = 120` but flagged once the limit drops below it.
#[test]
fn e501_honors_configured_line_length() {
    let engine = RuffEngine;
    let line = format!("x = \"{}\"\n", "a".repeat(94));
    assert_eq!(line.trim_end().len(), 100, "test fixture must be 100 chars");
    let src = SourceFile {
        path: std::path::PathBuf::from("mod.py"),
        language: Language::Python,
        content: line.clone().into(),
    };

    let mut wide = toml::Table::new();
    wide.insert("select".to_string(), code_array(&["E501"]));
    wide.insert("line_length".to_string(), toml::Value::Integer(120));
    let wide_diags = engine
        .lint(
            &src,
            &EngineConfig {
                options: wide,
                ..engine_cfg()
            },
        )
        .unwrap();
    assert!(
        !wide_diags.iter().any(|d| d.code.as_deref() == Some("E501")),
        "E501 must not fire on a 100-char line when line_length=120; got: {wide_diags:?}"
    );

    let mut narrow = toml::Table::new();
    narrow.insert("select".to_string(), code_array(&["E501"]));
    narrow.insert("line_length".to_string(), toml::Value::Integer(80));
    let narrow_diags = engine
        .lint(
            &src,
            &EngineConfig {
                options: narrow,
                ..engine_cfg()
            },
        )
        .unwrap();
    assert!(
        narrow_diags.iter().any(|d| d.code.as_deref() == Some("E501")),
        "E501 must fire on a 100-char line when line_length=80; got: {narrow_diags:?}"
    );
}
