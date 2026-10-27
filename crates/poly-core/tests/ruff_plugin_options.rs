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

const CASES: &[Case] = &[
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
