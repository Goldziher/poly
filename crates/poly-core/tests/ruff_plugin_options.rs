//! `[lint.python.ruff]` plugin and fix-policy options: each test lints one
//! source with and without the option and asserts the finding appears or
//! disappears, so a key that is accepted but ignored fails here.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Diagnostic, Engine, SourceFile},
    engines::ruff::RuffEngine,
};

type Options = Vec<(&'static str, toml::Value)>;

fn strings(items: &[&str]) -> toml::Value {
    toml::Value::Array(items.iter().map(|i| toml::Value::String((*i).into())).collect())
}

fn text(value: &str) -> toml::Value {
    toml::Value::String(value.into())
}

fn lint(content: &str, options: Options) -> Vec<Diagnostic> {
    let config = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: options.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    };
    let src = SourceFile {
        path: "fixture.py".into(),
        language: Language::Python,
        content: content.into(),
    };
    RuffEngine.lint(&src, &config).unwrap()
}

fn fires(code: &str, content: &str, options: Options) -> bool {
    lint(content, options).iter().any(|d| d.code.as_deref() == Some(code))
}

struct Case {
    name: &'static str,
    code: &'static str,
    source: &'static str,
    base: fn() -> Options,
    tuned: fn() -> Options,
    fires_when_tuned: bool,
}

fn select(code: &'static str) -> (&'static str, toml::Value) {
    ("select", strings(&[code]))
}

fn preview() -> (&'static str, toml::Value) {
    ("preview", toml::Value::Boolean(true))
}

const CASES: &[Case] = &[
    Case {
        name: "preview enables a preview-only rule",
        code: "PLR0914",
        source: "def f():\n    a = 1\n    b = 2\n    c = 3\n    return a + b + c\n",
        base: || vec![select("PLR0914"), ("pylint_max_locals", toml::Value::Integer(2))],
        tuned: || {
            vec![
                select("PLR0914"),
                ("pylint_max_locals", toml::Value::Integer(2)),
                preview(),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "dummy_variable_rgx silences F841 for a matching name",
        code: "F841",
        source: "def f():\n    scratch = 1\n",
        base: Vec::new,
        tuned: || vec![("dummy_variable_rgx", text("^scratch$"))],
        fires_when_tuned: false,
    },
    Case {
        name: "pycodestyle_max_line_length overrides line_length for E501",
        code: "E501",
        source: "x = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
        base: || vec![select("E501"), ("line_length", toml::Value::Integer(120))],
        tuned: || {
            vec![
                select("E501"),
                ("line_length", toml::Value::Integer(120)),
                ("pycodestyle_max_line_length", toml::Value::Integer(30)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "pycodestyle_ignore_overlong_task_comments exempts TODO comments",
        code: "E501",
        source: "# TODO: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        base: || {
            vec![
                select("E501"),
                ("pycodestyle_max_line_length", toml::Value::Integer(30)),
            ]
        },
        tuned: || {
            vec![
                select("E501"),
                ("pycodestyle_max_line_length", toml::Value::Integer(30)),
                ("pycodestyle_ignore_overlong_task_comments", toml::Value::Boolean(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "pep8_naming_extend_ignore_names exempts a function name",
        code: "N802",
        source: "def Handler():\n    pass\n",
        base: || vec![select("N802")],
        tuned: || {
            vec![
                select("N802"),
                ("pep8_naming_extend_ignore_names", strings(&["Handler"])),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "pep8_naming_classmethod_decorators accepts cls on a custom decorator",
        code: "N805",
        source: "class A:\n    @declared_attr\n    def f(cls):\n        pass\n",
        base: || vec![select("N805")],
        tuned: || {
            vec![
                select("N805"),
                ("pep8_naming_classmethod_decorators", strings(&["declared_attr"])),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_quotes_inline_quotes flips the preferred quote",
        code: "Q000",
        source: "x = 'a'\n",
        base: || vec![select("Q000")],
        tuned: || vec![select("Q000"), ("flake8_quotes_inline_quotes", text("single"))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_quotes_inline_quotes single flags double quotes",
        code: "Q000",
        source: "x = \"a\"\n",
        base: || vec![select("Q000")],
        tuned: || vec![select("Q000"), ("flake8_quotes_inline_quotes", text("single"))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_quotes_avoid_escape false flags an escape-avoiding quote",
        code: "Q003",
        source: "x = 'it\\'s'\n",
        base: || vec![select("Q003"), ("flake8_quotes_inline_quotes", text("single"))],
        tuned: || {
            vec![
                select("Q003"),
                ("flake8_quotes_inline_quotes", text("single")),
                ("flake8_quotes_avoid_escape", toml::Value::Boolean(false)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_type_checking_exempt_modules exempts a stdlib import used only for typing",
        code: "TC003",
        source: "from __future__ import annotations\n\nfrom collections import OrderedDict\n\n\ndef f(x: OrderedDict) -> None:\n    pass\n",
        base: || vec![select("TC003")],
        tuned: || {
            vec![
                select("TC003"),
                ("flake8_type_checking_exempt_modules", strings(&["collections"])),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_pytest_style_fixture_parentheses flips PT001",
        code: "PT001",
        source: "import pytest\n\n\n@pytest.fixture()\ndef f():\n    pass\n",
        base: || vec![select("PT001")],
        tuned: || {
            vec![
                select("PT001"),
                ("flake8_pytest_style_fixture_parentheses", toml::Value::Boolean(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_pytest_style_parametrize_names_type flips PT006",
        code: "PT006",
        source: "import pytest\n\n\n@pytest.mark.parametrize(('a', 'b'), [(1, 2)])\ndef test_x(a, b):\n    pass\n",
        base: || vec![select("PT006")],
        tuned: || {
            vec![
                select("PT006"),
                ("flake8_pytest_style_parametrize_names_type", text("csv")),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "external stops RUF102 from reporting a foreign noqa code",
        code: "RUF102",
        source: "import os  # noqa: WPS433\n\nprint(os)\n",
        base: || vec![("extend_select", strings(&["RUF102"]))],
        tuned: || vec![("extend_select", strings(&["RUF102"])), ("external", strings(&["WPS"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "pycodestyle_max_doc_length enables W505 at the configured width",
        code: "W505",
        source: "# aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        base: || vec![select("W505")],
        tuned: || vec![select("W505"), ("pycodestyle_max_doc_length", toml::Value::Integer(30))],
        fires_when_tuned: true,
    },
    Case {
        name: "pep8_naming_ignore_names replaces the default ignore list",
        code: "N802",
        source: "def setUp():
    pass
",
        base: || vec![select("N802")],
        tuned: || vec![select("N802"), ("pep8_naming_ignore_names", strings(&["other"]))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_quotes_multiline_quotes flips Q001",
        code: "Q001",
        source: "x = \'\'\'a\nb\'\'\'\n",
        base: || vec![select("Q001")],
        tuned: || vec![select("Q001"), ("flake8_quotes_multiline_quotes", text("single"))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_quotes_docstring_quotes flips Q002",
        code: "Q002",
        source: "def f():\n    \'\'\'doc\'\'\'\n",
        base: || vec![select("Q002")],
        tuned: || vec![select("Q002"), ("flake8_quotes_docstring_quotes", text("single"))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_type_checking_runtime_evaluated_base_classes keeps a typing-only import at runtime",
        code: "TC003",
        source: "from __future__ import annotations\n\nfrom decimal import Decimal\n\nfrom pydantic import BaseModel\n\n\nclass M(BaseModel):\n    x: Decimal\n",
        base: || vec![select("TC003")],
        tuned: || {
            vec![
                select("TC003"),
                (
                    "flake8_type_checking_runtime_evaluated_base_classes",
                    strings(&["pydantic.BaseModel"]),
                ),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_pytest_style_mark_parentheses flips PT023",
        code: "PT023",
        source: "import pytest\n\n\n@pytest.mark.slow()\ndef test_x():\n    pass\n",
        base: || vec![select("PT023")],
        tuned: || {
            vec![
                select("PT023"),
                ("flake8_pytest_style_mark_parentheses", toml::Value::Boolean(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_pytest_style_parametrize_values_type flips PT007",
        code: "PT007",
        source: "import pytest\n\n\n@pytest.mark.parametrize(\"a\", (1, 2))\ndef test_x(a):\n    pass\n",
        base: || vec![select("PT007")],
        tuned: || {
            vec![
                select("PT007"),
                ("flake8_pytest_style_parametrize_values_type", text("tuple")),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_pytest_style_parametrize_values_row_type flips PT007",
        code: "PT007",
        source: "import pytest\n\n\n@pytest.mark.parametrize((\"a\", \"b\"), [[1, 2], [3, 4]])\ndef test_x(a, b):\n    pass\n",
        base: || vec![select("PT007")],
        tuned: || {
            vec![
                select("PT007"),
                ("flake8_pytest_style_parametrize_values_row_type", text("list")),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "pylint_max_locals lowers PLR0914",
        code: "PLR0914",
        source: "def f():\n    a = 1\n    b = 2\n    c = 3\n    return a + b + c\n",
        base: || vec![select("PLR0914"), preview()],
        tuned: || {
            vec![
                select("PLR0914"),
                preview(),
                ("pylint_max_locals", toml::Value::Integer(2)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "pylint_max_statements lowers PLR0915",
        code: "PLR0915",
        source: "def f():\n    a = 1\n    b = 2\n    c = 3\n    return a + b + c\n",
        base: || vec![select("PLR0915")],
        tuned: || vec![select("PLR0915"), ("pylint_max_statements", toml::Value::Integer(2))],
        fires_when_tuned: true,
    },
    Case {
        name: "pylint_max_bool_expr lowers PLR0916",
        code: "PLR0916",
        source: "def f(a, b, c):\n    if a and b and c:\n        return 1\n    return 0\n",
        base: || vec![select("PLR0916"), preview()],
        tuned: || {
            vec![
                select("PLR0916"),
                preview(),
                ("pylint_max_bool_expr", toml::Value::Integer(1)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "pylint_max_nested_blocks lowers PLR1702",
        code: "PLR1702",
        source: "def f(x):\n    for a in x:\n        for b in a:\n            for c in b:\n                print(c)\n",
        base: || vec![select("PLR1702"), preview()],
        tuned: || {
            vec![
                select("PLR1702"),
                preview(),
                ("pylint_max_nested_blocks", toml::Value::Integer(2)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "pylint_max_public_methods lowers PLR0904",
        code: "PLR0904",
        source: "class C:\n    def a(self): ...\n    def b(self): ...\n    def c(self): ...\n",
        base: || vec![select("PLR0904"), preview()],
        tuned: || {
            vec![
                select("PLR0904"),
                preview(),
                ("pylint_max_public_methods", toml::Value::Integer(2)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "isort_combine_as_imports merges as-imports",
        code: "I001",
        source: "from a import b as c\nfrom a import d\n",
        base: || vec![select("I001")],
        tuned: || vec![select("I001"), ("isort_combine_as_imports", toml::Value::Boolean(true))],
        fires_when_tuned: true,
    },
    Case {
        name: "isort_force_single_line splits a multi-name import",
        code: "I001",
        source: "from a import b, c\n",
        base: || vec![select("I001")],
        tuned: || vec![select("I001"), ("isort_force_single_line", toml::Value::Boolean(true))],
        fires_when_tuned: true,
    },
    Case {
        name: "isort_force_sort_within_sections interleaves import styles",
        code: "I001",
        source: "import b\nfrom a import x\n",
        base: || vec![select("I001")],
        tuned: || {
            vec![
                select("I001"),
                ("isort_force_sort_within_sections", toml::Value::Boolean(true)),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "isort_lines_after_imports demands blank lines",
        code: "I001",
        source: "import os\n\nx = os.sep\n",
        base: || vec![select("I001")],
        tuned: || vec![select("I001"), ("isort_lines_after_imports", toml::Value::Integer(2))],
        fires_when_tuned: true,
    },
    Case {
        name: "isort_order_by_type false sorts names alphabetically",
        code: "I001",
        source: "from a import Zed, apple\n",
        base: || vec![select("I001")],
        tuned: || vec![select("I001"), ("isort_order_by_type", toml::Value::Boolean(false))],
        fires_when_tuned: true,
    },
    Case {
        name: "known_local_folder splits a section off the third-party block",
        code: "I001",
        source: "import requests\n\nimport zlocal\n",
        base: || vec![select("I001")],
        tuned: || vec![select("I001"), ("known_local_folder", strings(&["zlocal"]))],
        fires_when_tuned: false,
    },
];

#[test]
fn each_option_changes_the_finding_it_governs() {
    for case in CASES {
        let baseline = fires(case.code, case.source, (case.base)());
        let tuned = fires(case.code, case.source, (case.tuned)());
        assert_eq!(
            tuned, case.fires_when_tuned,
            "{}: with the option {} should be {}",
            case.name, case.code, case.fires_when_tuned
        );
        assert_ne!(
            baseline, tuned,
            "{}: the option must change whether {} fires (baseline {baseline})",
            case.name, case.code
        );
    }
}

fn fix_of(code: &str, content: &str, options: Options) -> usize {
    lint(content, options)
        .iter()
        .find(|d| d.code.as_deref() == Some(code))
        .unwrap_or_else(|| panic!("{code} must be reported"))
        .fix
        .len()
}

const UNUSED_IMPORT: &str = "import os\n";

#[test]
fn a_rule_reports_with_a_fix_by_default() {
    assert!(fix_of("F401", UNUSED_IMPORT, Vec::new()) > 0);
}

#[test]
fn unfixable_removes_the_fix_but_keeps_the_finding() {
    assert_eq!(
        fix_of("F401", UNUSED_IMPORT, vec![("unfixable", strings(&["F401"]))]),
        0
    );
}

#[test]
fn fixable_limits_fixes_to_the_listed_rules() {
    assert_eq!(
        fix_of("F401", UNUSED_IMPORT, vec![("fixable", strings(&["I"]))]),
        0,
        "F401 is outside `fixable`"
    );
    assert!(
        fix_of("F401", UNUSED_IMPORT, vec![("fixable", strings(&["F401"]))]) > 0,
        "F401 is inside `fixable`"
    );
}

#[test]
fn fixable_all_leaves_every_fix_in_place() {
    assert!(fix_of("F401", UNUSED_IMPORT, vec![("fixable", strings(&["ALL"]))]) > 0);
}

#[test]
fn unfixable_wins_over_fixable() {
    let options = vec![("fixable", strings(&["ALL"])), ("unfixable", strings(&["F401"]))];
    assert_eq!(fix_of("F401", UNUSED_IMPORT, options), 0);
}

const UNUSED_ASSIGNMENT: &str = "def f():\n    scratch = print(1)\n";

#[test]
fn an_unsafe_fix_is_withheld_unless_unsafe_fixes_is_on() {
    assert_eq!(fix_of("F841", UNUSED_ASSIGNMENT, Vec::new()), 0);
    assert!(
        fix_of(
            "F841",
            UNUSED_ASSIGNMENT,
            vec![("unsafe_fixes", toml::Value::Boolean(true))]
        ) > 0
    );
}

#[test]
fn unsafe_fixes_false_keeps_unsafe_fixes_withheld() {
    let options = vec![("unsafe_fixes", toml::Value::Boolean(false))];
    assert_eq!(fix_of("F841", UNUSED_ASSIGNMENT, options), 0);
}

#[test]
fn unsafe_fixes_leaves_safe_fixes_alone() {
    let options = vec![("unsafe_fixes", toml::Value::Boolean(true))];
    assert!(fix_of("F401", UNUSED_IMPORT, options) > 0);
}
