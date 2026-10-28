//! `[lint.python.ruff]` keys for the plugin tables beyond the first batch
//! (`ruff_plugin_options.rs`): each test lints one source with and without the
//! option and asserts the finding appears or disappears, so a key that is
//! accepted but ignored fails here.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::ruff::RuffEngine,
};

type Options = Vec<(&'static str, toml::Value)>;

fn strings(items: &[&str]) -> toml::Value {
    toml::Value::Array(items.iter().map(|i| toml::Value::String((*i).into())).collect())
}

fn text(value: &str) -> toml::Value {
    toml::Value::String(value.into())
}

fn flag(value: bool) -> toml::Value {
    toml::Value::Boolean(value)
}

fn int(value: i64) -> toml::Value {
    toml::Value::Integer(value)
}

fn table(entries: &[(&str, toml::Value)]) -> toml::Value {
    toml::Value::Table(entries.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect())
}

fn config(options: Options) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: options.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn source(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Python,
        content: content.into(),
    }
}

fn fires(code: &str, content: &str, options: Options) -> bool {
    RuffEngine
        .lint(&source("fixture.py", content), &config(options))
        .unwrap()
        .iter()
        .any(|d| d.code.as_deref() == Some(code))
}

fn select(code: &'static str) -> (&'static str, toml::Value) {
    ("select", strings(&[code]))
}

fn preview() -> (&'static str, toml::Value) {
    ("preview", flag(true))
}

struct Case {
    name: &'static str,
    code: &'static str,
    source: &'static str,
    base: fn() -> Options,
    tuned: fn() -> Options,
    fires_when_tuned: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "builtins declares an extra builtin name",
        code: "F821",
        source: "print(custom_builtin)\n",
        base: || vec![select("F821")],
        tuned: || vec![select("F821"), ("builtins", strings(&["custom_builtin"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "logger_objects makes a custom object a logger",
        code: "G002",
        source: "from app import mylog\n\nmylog.info(\"%s\" % 1)\n",
        base: || vec![select("G002")],
        tuned: || vec![select("G002"), ("logger_objects", strings(&["app.mylog"]))],
        fires_when_tuned: true,
    },
    Case {
        name: "task_tags exempts a custom tag from commented-out code",
        code: "ERA001",
        source: "# MYTAG: x = 1\n",
        base: || vec![select("ERA001")],
        tuned: || vec![select("ERA001"), ("task_tags", strings(&["MYTAG"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "typing_modules treats another module as typing",
        code: "UP006",
        source: "from mylib.typing import List\n\nx: List[int] = []\n",
        base: || vec![select("UP006"), ("target_version", text("py39"))],
        tuned: || {
            vec![
                select("UP006"),
                ("target_version", text("py39")),
                ("typing_modules", strings(&["mylib.typing"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "allowed_confusables exempts a look-alike character",
        code: "RUF001",
        source: "x = \"\u{3c1}\"\n",
        base: || vec![select("RUF001")],
        tuned: || vec![select("RUF001"), ("allowed_confusables", strings(&["\u{3c1}"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "isort_required_imports demands an import",
        code: "I002",
        source: "x = 1\n",
        base: || vec![select("I002")],
        tuned: || {
            vec![
                select("I002"),
                ("isort_required_imports", strings(&["from __future__ import annotations"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_annotations_mypy_init_return allows an unannotated __init__ return",
        code: "ANN204",
        source: "class C:\n    def __init__(self, a: int):\n        pass\n",
        base: || vec![select("ANN204")],
        tuned: || vec![select("ANN204"), ("flake8_annotations_mypy_init_return", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_annotations_suppress_none_returning allows a function that returns nothing",
        code: "ANN201",
        source: "def f(a: int):\n    pass\n",
        base: || vec![select("ANN201")],
        tuned: || vec![select("ANN201"), ("flake8_annotations_suppress_none_returning", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_annotations_allow_star_arg_any allows *args: Any",
        code: "ANN401",
        source: "from typing import Any\n\n\ndef f(*args: Any) -> None:\n    pass\n",
        base: || vec![select("ANN401")],
        tuned: || vec![select("ANN401"), ("flake8_annotations_allow_star_arg_any", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_annotations_suppress_dummy_args allows an unannotated _",
        code: "ANN001",
        source: "def f(_) -> None:\n    pass\n",
        base: || vec![select("ANN001")],
        tuned: || vec![select("ANN001"), ("flake8_annotations_suppress_dummy_args", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_annotations_ignore_fully_untyped skips a function with no annotations",
        code: "ANN001",
        source: "def f(a):\n    pass\n",
        base: || vec![select("ANN001")],
        tuned: || vec![select("ANN001"), ("flake8_annotations_ignore_fully_untyped", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_bandit_hardcoded_tmp_directory replaces the temporary directories",
        code: "S108",
        source: "path = \"/mnt/scratch/data\"\n",
        base: || vec![select("S108")],
        tuned: || {
            vec![
                select("S108"),
                ("flake8_bandit_hardcoded_tmp_directory", strings(&["/mnt/scratch"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_bandit_hardcoded_tmp_directory_extend adds a temporary directory",
        code: "S108",
        source: "path = \"/mnt/scratch/data\"\n",
        base: || vec![select("S108")],
        tuned: || {
            vec![
                select("S108"),
                ("flake8_bandit_hardcoded_tmp_directory_extend", strings(&["/mnt/scratch"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_bandit_check_typed_exception flags try-except-pass on a typed exception",
        code: "S110",
        source: "try:\n    pass\nexcept ValueError:\n    pass\n",
        base: || vec![select("S110")],
        tuned: || vec![select("S110"), ("flake8_bandit_check_typed_exception", flag(true))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_boolean_trap_extend_allowed_calls exempts a call",
        code: "FBT003",
        source: "import mylib\n\nmylib.configure(True)\n",
        base: || vec![select("FBT003")],
        tuned: || {
            vec![
                select("FBT003"),
                ("flake8_boolean_trap_extend_allowed_calls", strings(&["mylib.configure"])),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_bugbear_extend_immutable_calls allows a call in a default",
        code: "B008",
        source: "from lib import make\n\n\ndef f(a=make()):\n    pass\n",
        base: || vec![select("B008")],
        tuned: || {
            vec![
                select("B008"),
                ("flake8_bugbear_extend_immutable_calls", strings(&["lib.make"])),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_builtins_ignorelist exempts a shadowed builtin",
        code: "A001",
        source: "id = 1\n",
        base: || vec![select("A001")],
        tuned: || vec![select("A001"), ("flake8_builtins_ignorelist", strings(&["id"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_comprehensions_allow_dict_calls_with_keyword_arguments allows dict(a=1)",
        code: "C408",
        source: "x = dict(a=1)\n",
        base: || vec![select("C408")],
        tuned: || {
            vec![
                select("C408"),
                ("flake8_comprehensions_allow_dict_calls_with_keyword_arguments", flag(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_copyright_min_file_size spares a small file",
        code: "CPY001",
        source: "x = 1\n",
        base: || vec![select("CPY001")],
        tuned: || vec![select("CPY001"), ("flake8_copyright_min_file_size", int(1000))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_copyright_notice_rgx changes what counts as a notice",
        code: "CPY001",
        source: "# Copyright 2024 Acme\nx = 1\n",
        base: || vec![select("CPY001")],
        tuned: || vec![select("CPY001"), ("flake8_copyright_notice_rgx", text("Licensed under"))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_copyright_author requires the notice to name the author",
        code: "CPY001",
        source: "# Copyright 2024 Other\nx = 1\n",
        base: || vec![select("CPY001")],
        tuned: || vec![select("CPY001"), ("flake8_copyright_author", text("Acme"))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_errmsg_max_string_length spares a short message",
        code: "EM101",
        source: "raise ValueError(\"short\")\n",
        base: || vec![select("EM101")],
        tuned: || vec![select("EM101"), ("flake8_errmsg_max_string_length", int(20))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_gettext_function_names replaces the gettext functions",
        code: "INT001",
        source: "def f(y):\n    return _(f\"x {y}\")\n",
        base: || vec![select("INT001")],
        tuned: || vec![select("INT001"), ("flake8_gettext_function_names", strings(&["tr"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_gettext_extend_function_names adds a gettext function",
        code: "INT001",
        source: "def f(y):\n    return tr(f\"x {y}\")\n",
        base: || vec![select("INT001")],
        tuned: || {
            vec![
                select("INT001"),
                ("flake8_gettext_extend_function_names", strings(&["tr"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_implicit_str_concat_allow_multiline false stops joining strings across lines",
        code: "ISC002",
        source: "x = (\"a\"\n     \"b\")\n",
        base: || vec![select("ISC002")],
        tuned: || vec![select("ISC002"), ("flake8_implicit_str_concat_allow_multiline", flag(false))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_self_ignore_names replaces the ignored private names",
        code: "SLF001",
        source: "def f(x):\n    return x._asdict()\n",
        base: || vec![select("SLF001")],
        tuned: || vec![select("SLF001"), ("flake8_self_ignore_names", strings(&["_other"]))],
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_self_extend_ignore_names exempts a private name",
        code: "SLF001",
        source: "def f(x):\n    return x._secret\n",
        base: || vec![select("SLF001")],
        tuned: || vec![select("SLF001"), ("flake8_self_extend_ignore_names", strings(&["_secret"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "flake8_tidy_imports_ban_relative_imports all bans a sibling import",
        code: "TID252",
        source: "from . import sibling\n",
        base: || vec![select("TID252")],
        tuned: || {
            vec![
                select("TID252"),
                ("flake8_tidy_imports_ban_relative_imports", text("all")),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_tidy_imports_banned_api bans a module",
        code: "TID251",
        source: "import cgi\n",
        base: || vec![select("TID251")],
        tuned: || {
            vec![
                select("TID251"),
                (
                    "flake8_tidy_imports_banned_api",
                    table(&[("cgi", table(&[("msg", text("deprecated"))]))]),
                ),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_tidy_imports_extend_banned_api bans a module",
        code: "TID251",
        source: "import cgi\n",
        base: || vec![select("TID251")],
        tuned: || {
            vec![
                select("TID251"),
                (
                    "flake8_tidy_imports_extend_banned_api",
                    table(&[("cgi", table(&[("msg", text("deprecated"))]))]),
                ),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_tidy_imports_banned_module_level_imports bans a top-level import",
        code: "TID253",
        source: "import torch\n",
        base: || vec![select("TID253")],
        tuned: || {
            vec![
                select("TID253"),
                ("flake8_tidy_imports_banned_module_level_imports", strings(&["torch"])),
            ]
        },
        fires_when_tuned: true,
    },
    Case {
        name: "flake8_unused_arguments_ignore_variadic_names spares *args",
        code: "ARG001",
        source: "def f(*args):\n    pass\n",
        base: || vec![select("ARG001")],
        tuned: || vec![select("ARG001"), ("flake8_unused_arguments_ignore_variadic_names", flag(true))],
        fires_when_tuned: false,
    },
    Case {
        name: "pydocstyle_ignore_decorators skips a decorated method",
        code: "D102",
        source: "from lib import custom\n\n\nclass C:\n    \"\"\"Doc.\"\"\"\n\n    @custom\n    def method(self):\n        pass\n",
        base: || vec![select("D102")],
        tuned: || vec![select("D102"), ("pydocstyle_ignore_decorators", strings(&["lib.custom"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "pydocstyle_property_decorators treats a decorator as a property",
        code: "D401",
        source: "from lib import cached\n\n\nclass C:\n    \"\"\"Doc.\"\"\"\n\n    @cached\n    def value(self):\n        \"\"\"Returns the value.\"\"\"\n        return 1\n",
        base: || vec![select("D401")],
        tuned: || vec![select("D401"), ("pydocstyle_property_decorators", strings(&["lib.cached"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "pydocstyle_ignore_var_parameters spares an undocumented *args",
        code: "D417",
        source: "def f(a, *args):\n    \"\"\"Do it.\n\n    Args:\n        a: The a.\n    \"\"\"\n",
        base: || vec![select("D417"), ("pydocstyle_convention", text("google"))],
        tuned: || {
            vec![
                select("D417"),
                ("pydocstyle_convention", text("google")),
                ("pydocstyle_ignore_var_parameters", flag(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "pydoclint_ignore_one_line_docstrings spares a one-line docstring",
        code: "DOC201",
        source: "def f():\n    \"\"\"Return one.\"\"\"\n    return 1\n",
        base: || vec![select("DOC201"), preview(), ("pydocstyle_convention", text("google"))],
        tuned: || {
            vec![
                select("DOC201"),
                preview(),
                ("pydocstyle_convention", text("google")),
                ("pydoclint_ignore_one_line_docstrings", flag(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "pyflakes_allowed_unused_imports spares a side-effect import",
        code: "F401",
        source: "import hvplot.pandas\n",
        base: || vec![select("F401")],
        tuned: || vec![select("F401"), ("pyflakes_allowed_unused_imports", strings(&["hvplot.pandas"]))],
        fires_when_tuned: false,
    },
    Case {
        name: "pyupgrade_keep_runtime_typing keeps typing.List under a future import",
        code: "UP006",
        source: "from __future__ import annotations\n\nfrom typing import List\n\nx: List[int] = []\n",
        base: || vec![select("UP006"), ("target_version", text("py39"))],
        tuned: || {
            vec![
                select("UP006"),
                ("target_version", text("py39")),
                ("pyupgrade_keep_runtime_typing", flag(true)),
            ]
        },
        fires_when_tuned: false,
    },
    Case {
        name: "ruff_parenthesize_tuple_in_subscript prefers parentheses",
        code: "RUF031",
        source: "d = {}\nd[1, 2]\n",
        base: || vec![select("RUF031"), preview()],
        tuned: || vec![select("RUF031"), preview(), ("ruff_parenthesize_tuple_in_subscript", flag(true))],
        fires_when_tuned: true,
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

#[test]
fn a_malformed_required_import_is_ignored_not_fatal() {
    let options = vec![select("I002"), ("isort_required_imports", strings(&["not an import"]))];
    assert!(!fires("I002", "x = 1\n", options));
}

#[test]
fn a_relative_required_import_is_demanded() {
    let options = vec![select("I002"), ("isort_required_imports", strings(&["from . import sibling"]))];
    assert!(fires("I002", "x = 1\n", options));
}

fn formatted(path: &str, options: Options) -> bool {
    matches!(
        RuffEngine.format(&source(path, "x  =  1\n"), &config(options)).unwrap(),
        FormatOutput::Formatted(_)
    )
}

#[test]
fn format_exclude_keeps_the_formatter_away_from_matching_files() {
    let exclude = || vec![("exclude", strings(&["gen"]))];
    assert!(formatted("pkg/gen/x.py", Vec::new()), "unexcluded files are formatted");
    assert!(!formatted("pkg/gen/x.py", exclude()), "a directory pattern covers its contents");
    assert!(formatted("pkg/src/x.py", exclude()), "other files are still formatted");
}

#[test]
fn format_exclude_accepts_a_file_glob() {
    let exclude = vec![("exclude", strings(&["*_pb2.py"]))];
    assert!(!formatted("pkg/msg_pb2.py", exclude));
}
