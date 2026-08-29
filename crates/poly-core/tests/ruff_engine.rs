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
    engine::{Engine, FormatOutput, Severity, SourceFile},
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

/// A fully annotated, idiomatic module must be silent under the opinionated
/// default set — the guard against the rule selection becoming unusably wide.
#[test]
fn valid_python_has_no_diagnostics() {
    let engine = RuffEngine;
    let src = make_src("ok.py", "def ok() -> int:\n    return 1\n");
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

/// Canonical `extend_select` adds a rule on top of the default set: a camelCase
/// function name is clean under the defaults (pep8-naming is not selected) but
/// trips N802 once `extend_select = ["N802"]` is applied.
#[test]
fn extend_select_adds_rule_beyond_defaults() {
    let engine = RuffEngine;
    let content = "def badName() -> int:\n    return 1\n";

    let base = engine.lint(&make_src("m.py", content), &engine_cfg()).unwrap();
    assert!(
        !base.iter().any(|d| d.code.as_deref() == Some("N802")),
        "N802 must not fire under the default rule set; got: {base:?}"
    );

    let mut options = toml::Table::new();
    options.insert("extend_select".to_string(), code_array(&["N802"]));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let extended = engine.lint(&make_src("m.py", content), &cfg).unwrap();
    assert!(
        extended.iter().any(|d| d.code.as_deref() == Some("N802")),
        "extend_select = [\"N802\"] must flag the camelCase name; got: {extended:?}"
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

// ── Opinionated default rule set (Phase 3.0) ─────────────────────────────────
//
// Each of these asserts a representative member of a newly-selected category
// actually fires with *no* engine config — the default path — and each was
// verified to fail with the previous `["F", "E4", "E7", "E9", "W6", "I", "UP",
// "B"]` selection.

/// Returns every rule code the default (no-config) path reports for `content`.
fn default_codes(content: &str) -> Vec<String> {
    RuffEngine
        .lint(&make_src("m.py", content), &engine_cfg())
        .unwrap()
        .into_iter()
        .filter_map(|d| d.code)
        .collect()
}

/// Typing (`ANN`): an unannotated public function is flagged ANN201 (missing
/// return type) and ANN001 (missing argument type) with no config at all.
#[test]
fn unannotated_function_trips_ann201_and_ann001() {
    let codes = default_codes("def handle(request):\n    return request\n");
    assert!(
        codes.iter().any(|c| c == "ANN201"),
        "an unannotated public function must trip ANN201; got: {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "ANN001"),
        "an unannotated argument must trip ANN001; got: {codes:?}"
    );
}

/// Error handling (`BLE` / `S110`): the swallowed-exception pattern —
/// `except Exception: pass` — is reported by both blind-except and bandit.
#[test]
fn swallowed_exception_trips_ble001_and_s110() {
    let codes = default_codes("def run() -> None:\n    try:\n        boom()\n    except Exception:\n        pass\n");
    assert!(
        codes.iter().any(|c| c == "BLE001"),
        "`except Exception:` must trip BLE001; got: {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "S110"),
        "`except …: pass` must trip S110; got: {codes:?}"
    );
}

/// Functional style (`SIM` / `C4` / `RET`): collapsible `if`, a list()
/// round-trip around a comprehension, and a superfluous `else` after `return`.
#[test]
fn functional_style_categories_fire() {
    let sim = default_codes("def f(a: bool, b: bool) -> None:\n    if a:\n        if b:\n            pass\n");
    assert!(
        sim.iter().any(|c| c.starts_with("SIM")),
        "nested collapsible ifs must trip a SIM rule; got: {sim:?}"
    );

    let c4 = default_codes("def f(xs: list[int]) -> list[int]:\n    return list([x for x in xs])\n");
    assert!(
        c4.iter().any(|c| c.starts_with("C4")),
        "list() around a comprehension must trip a C4 rule; got: {c4:?}"
    );

    let ret = default_codes("def f(a: bool) -> int:\n    if a:\n        return 1\n    else:\n        return 2\n");
    assert!(
        ret.iter().any(|c| c.starts_with("RET")),
        "`else` after `return` must trip a RET rule; got: {ret:?}"
    );
}

/// Hygiene (`T20`): a stray `print` in shipped code is reported by default.
/// This is the rule `extend_select_adds_rule_beyond_defaults` used to need
/// `extend_select` to reach.
#[test]
fn stray_print_trips_t201_by_default() {
    let codes = default_codes("def f() -> None:\n    print(1)\n");
    assert!(
        codes.iter().any(|c| c == "T201"),
        "print() must trip T201 under the default set; got: {codes:?}"
    );
}

/// Complexity (`C90` / `PLR`): selecting these makes the *already-wired*
/// `mccabe_max_complexity` / `pylint_max_*` options take effect. Proven by
/// tightening the threshold from config and watching C901 appear.
#[test]
fn c901_is_selected_by_default_and_threshold_config_applies() {
    let branchy = "def f(x: int) -> int:\n    if x == 1:\n        return 1\n    if x == 2:\n        return 2\n    if x == 3:\n        return 3\n    return 0\n";

    let default = default_codes(branchy);
    assert!(
        !default.iter().any(|c| c == "C901"),
        "C901 must not fire below ruff's default complexity threshold; got: {default:?}"
    );

    let mut options = toml::Table::new();
    options.insert("mccabe_max_complexity".to_string(), toml::Value::Integer(1));
    let tightened = RuffEngine
        .lint(
            &make_src("m.py", branchy),
            &EngineConfig {
                options,
                ..engine_cfg()
            },
        )
        .unwrap();
    assert!(
        tightened.iter().any(|d| d.code.as_deref() == Some("C901")),
        "`mccabe_max_complexity = 1` alone must now surface C901 — C90 is selected by default; got: {tightened:?}"
    );
}

/// The tuned-out rules stay off by default and come back with `extend_select`.
///
/// Each was measured on the corpus and rejected as noise (see
/// `DEFAULT_IGNORED_RULES` in `engines/ruff.rs`); this asserts both halves of
/// that contract — silent by default, reachable on request.
#[test]
fn tuned_out_rules_are_off_by_default_but_reenableable() {
    // (code, source that trips it) — one per entry in `DEFAULT_IGNORED_RULES`.
    let cases: Vec<(&str, String)> = vec![
        (
            "B008",
            "import datetime\n\n\ndef f(a: datetime.datetime = datetime.datetime.now()) -> None:\n    return None\n"
                .to_string(),
        ),
        ("RUF100", "x = 1  # noqa: F401\n".to_string()),
        ("ANN002", "def f(*args) -> None:\n    return None\n".to_string()),
        ("ANN003", "def f(**kwargs) -> None:\n    return None\n".to_string()),
        (
            "ARG002",
            "class C:\n    def m(self, unused: int) -> None:\n        return None\n".to_string(),
        ),
        ("PLR2004", "def f(x: int) -> bool:\n    return x == 42\n".to_string()),
        (
            "TRY003",
            "def f() -> None:\n    raise ValueError(\"a message long enough to trip TRY003\")\n".to_string(),
        ),
    ];

    for (code, content) in cases {
        let default = default_codes(&content);
        assert!(
            !default.iter().any(|c| c == code),
            "{code} must be off by default; got: {default:?}"
        );

        let mut options = toml::Table::new();
        options.insert("extend_select".to_string(), code_array(&[code]));
        let reenabled = RuffEngine
            .lint(
                &make_src("m.py", &content),
                &EngineConfig {
                    options,
                    ..engine_cfg()
                },
            )
            .unwrap();
        assert!(
            reenabled.iter().any(|d| d.code.as_deref() == Some(code)),
            "extend_select = [\"{code}\"] must bring it back; got: {reenabled:?}"
        );
    }
}

/// ANN401 (`Any` is disallowed) is *not* tuned out: it is the Python analogue of
/// the `typescript/no-explicit-any` poly enables for oxlint, and was measured at
/// 0.64 findings per Python file with 9 of 20 hand-read hits genuinely
/// narrowable. It must fire on the default path.
#[test]
fn ann401_fires_by_default() {
    let codes = default_codes("from typing import Any\n\n\ndef f(x: Any) -> None:\n    return None\n");
    assert!(
        codes.iter().any(|c| c == "ANN401"),
        "`x: Any` must trip ANN401 under the default set; got: {codes:?}"
    );
}

/// The `EM` category (exception message assigned before the `raise`) is not
/// selected at all — it was measured at 0.51 findings per Python file across
/// every repo in the corpus and rejected.
#[test]
fn em_category_is_not_selected() {
    let codes = default_codes("def f() -> None:\n    raise ValueError(\"boom\")\n");
    assert!(
        !codes.iter().any(|c| c.starts_with("EM")),
        "the EM category must not be selected; got: {codes:?}"
    );
}

/// Advisory categories are reported at `Warning`, not ruff's own `Error`.
///
/// `poly lint` exits non-zero only on error-severity findings, so this mapping is
/// what makes the opinionated set a guard rail rather than a gate. Without it, a
/// repo upgrading poly discovers its CI is red because its functions lack return
/// annotations. Locked in per rule family, since a single missed prefix in
/// `ADVISORY_RULE_PREFIXES` silently re-arms the gate for that family.
#[test]
fn advisory_categories_are_warnings_not_errors() {
    let cases: Vec<(&str, String)> = vec![
        ("ANN201", "def f():\n    return 1\n".to_string()),
        (
            "BLE001",
            "def f() -> None:\n    try:\n        pass\n    except Exception:\n        return None\n".to_string(),
        ),
        (
            "S110",
            "def f() -> None:\n    try:\n        pass\n    except Exception:\n        pass\n".to_string(),
        ),
        (
            "SIM105",
            "def f() -> None:\n    try:\n        pass\n    except ValueError:\n        pass\n".to_string(),
        ),
        (
            "C901",
            format!(
                "def f(a: int) -> int:\n{}    return a\n",
                "    if a:\n        a += 1\n".repeat(24)
            ),
        ),
        ("T201", "def f() -> None:\n    print(\"hi\")\n".to_string()),
        ("ARG001", "def f(unused: int) -> None:\n    return None\n".to_string()),
    ];

    for (code, source) in &cases {
        let diagnostics = RuffEngine.lint(&make_src("m.py", source), &engine_cfg()).unwrap();
        let found = diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some(*code))
            .unwrap_or_else(|| panic!("{code} must fire on this source; got: {diagnostics:?}"));
        assert_eq!(
            found.severity,
            Severity::Warning,
            "{code} is advisory and must not fail a run"
        );
    }
}

/// The correctness core keeps ruff's `Error` severity, so real bugs still gate.
///
/// The companion to `advisory_categories_are_warnings_not_errors`: downgrading
/// everything would be just as wrong as downgrading nothing.
#[test]
fn correctness_rules_stay_errors() {
    let diagnostics = RuffEngine
        .lint(&make_src("m.py", "import os\n\nx = undefined_name\n"), &engine_cfg())
        .unwrap();

    for code in ["F821", "F401"] {
        let found = diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some(code))
            .unwrap_or_else(|| panic!("{code} must fire; got: {diagnostics:?}"));
        assert_eq!(
            found.severity,
            Severity::Error,
            "{code} is a real defect and must keep failing the run"
        );
    }
}
