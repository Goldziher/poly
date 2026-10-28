use super::*;
use tempfile::tempdir;
use toml_edit::DocumentMut;

fn rendered(dir: &Path) -> (String, Absorb) {
    let result = import(dir).expect("ruff config present");
    let mut doc = DocumentMut::new();
    super::super::apply(&mut doc, &result.fragments);
    (doc.to_string(), result.absorb)
}

#[test]
fn absorbs_pyproject_ruff_select_ignore_and_per_file() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("pyproject.toml"),
        r#"
[tool.ruff]
line-length = 100
target-version = "py311"

[tool.ruff.lint]
select = ["ALL"]
ignore = ["D203", "COM812"]

[tool.ruff.lint.pydocstyle]
convention = "google"

[tool.ruff.lint.per-file-ignores]
"tests/**" = ["S101", "ANN"]
"__init__.py" = ["F401"]
"#,
    )
    .unwrap();
    let (toml, absorb) = rendered(dir.path());
    assert_eq!(absorb, Absorb::Full);
    insta::assert_snapshot!("ruff_pyproject_full", toml);
}

#[test]
fn flat_ruff_toml_with_unknown_key_is_partial() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("ruff.toml"),
        "select = [\"E\", \"F\"]\npreview = true\n",
    )
    .unwrap();
    let result = import(dir.path()).unwrap();
    assert_eq!(result.absorb, Absorb::Partial(vec!["preview".to_string()]));
}

#[test]
fn extend_exclude_folds_into_discovery_and_is_full() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("ruff.toml"),
        "select = [\"E\"]\nextend-exclude = [\"build\", \"vendor\"]\n",
    )
    .unwrap();
    let (toml, absorb) = rendered(dir.path());
    assert_eq!(absorb, Absorb::Full, "extend-exclude is now representable");
    assert!(
        toml.contains("[discovery]"),
        "exclude globs go under [discovery]: {toml}"
    );
    assert!(toml.contains("build") && toml.contains("vendor"), "{toml}");
}

#[test]
fn format_style_keys_are_carried_into_the_formatter_table() {
    let (result, doc) = migrate_pyproject(
        r#"
[tool.ruff]
line-length = 100
[tool.ruff.format]
quote-style = "single"
indent-style = "tab"
line-ending = "lf"
skip-magic-trailing-comma = true
"#,
    );
    assert_eq!(result.absorb, Absorb::Full);
    let fmt = &doc["fmt"]["python"]["ruff"];
    assert_eq!(fmt["quote_style"].as_str(), Some("single"));
    assert_eq!(fmt["indent_style"].as_str(), Some("tab"));
    assert_eq!(fmt["line_ending"].as_str(), Some("lf"));
    assert_eq!(fmt["skip_magic_trailing_comma"].as_bool(), Some(true));
}

#[test]
fn format_preview_and_exclude_are_carried_into_the_formatter_table() {
    let (result, doc) = migrate_pyproject("[tool.ruff.format]\npreview = true\nexclude = [\"gen/**\"]\n");
    assert_eq!(result.absorb, Absorb::Full);
    let fmt = &doc["fmt"]["python"]["ruff"];
    assert_eq!(fmt["preview"].as_bool(), Some(true));
    assert_eq!(fmt["exclude"][0].as_str(), Some("gen/**"));
}

#[test]
fn format_keys_poly_cannot_honor_keep_the_source() {
    let (result, _) = migrate_pyproject("[tool.ruff.format]\nnested-string-quote-style = \"preferred\"\n");
    assert_eq!(
        result.absorb,
        Absorb::Partial(vec!["format.nested-string-quote-style".to_string()])
    );
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::create_dir_all(dir.join(name).parent().unwrap()).unwrap();
    std::fs::write(dir.join(name), content).unwrap();
}

#[test]
fn a_missing_target_version_is_inferred_from_requires_python() {
    for (requires, expected) in [
        (">=3.11", "py311"),
        (">=3.9,<4", "py39"),
        ("~=3.10.2", "py310"),
        (">3.10", "py311"),
        ("==3.12.*", "py312"),
    ] {
        let (_, doc) = migrate_pyproject(&format!(
            "[project]\nrequires-python = \"{requires}\"\n[tool.ruff.lint]\nselect = [\"E\"]\n"
        ));
        assert_eq!(
            doc["lint"]["python"]["ruff"]["target_version"].as_str(),
            Some(expected),
            "{requires}"
        );
    }
}

#[test]
fn an_explicit_target_version_beats_requires_python() {
    let (_, doc) =
        migrate_pyproject("[project]\nrequires-python = \">=3.11\"\n[tool.ruff]\ntarget-version = \"py38\"\n");
    assert_eq!(doc["lint"]["python"]["ruff"]["target_version"].as_str(), Some("py38"));
}

#[test]
fn ruff_defaults_that_poly_would_replace_are_pinned() {
    let (_, doc) = migrate_pyproject("[tool.ruff.lint]\nselect = [\"E\"]\n");
    assert_eq!(doc["lint"]["python"]["ruff"]["line_length"].as_integer(), Some(88));
    let fmt = &doc["fmt"]["python"]["ruff"];
    assert_eq!(fmt["line_length"].as_integer(), Some(88));
    assert_eq!(fmt["docstring_code_format"].as_bool(), Some(false));
    assert_eq!(fmt["line_ending"].as_str(), Some("auto"));
    let (_, enabled) = migrate_pyproject("[tool.ruff.format]\ndocstring-code-format = true\n");
    assert_eq!(
        enabled["fmt"]["python"]["ruff"]["docstring_code_line_length"].as_str(),
        Some("dynamic"),
        "ruff's width follows the code unless told otherwise"
    );
}

#[test]
fn a_per_file_ignore_without_a_slash_applies_at_every_depth() {
    let (_, doc) = migrate_pyproject(
        "[tool.ruff.lint.per-file-ignores]\n\"__init__.py\" = [\"F401\"]\n\"tests/*\" = [\"S\"]\n\"{a,b}.py\" = [\"E\"]\n",
    );
    let ignores = doc["per-file-ignores"].as_table().unwrap();
    assert!(ignores.contains_key("**/__init__.py"), "{ignores:?}");
    assert!(
        ignores.contains_key("tests/*"),
        "a pattern with a directory stays anchored"
    );
    assert!(ignores.contains_key("**/{a,b}.py"));
}

#[test]
fn extend_resolves_the_base_with_own_settings_winning_and_extend_lists_adding() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.ruff]\nline-length = 100\n[tool.ruff.lint]\nselect = [\"E\"]\nignore = [\"E501\"]\nextend-select = [\"B\"]\n\
             [tool.ruff.lint.per-file-ignores]\n\"a.py\" = [\"F401\"]\n",
    );
    write(
        dir.path(),
        "svc/ruff.toml",
        "extend = \"../pyproject.toml\"\nline-length = 120\n[lint]\nextend-select = [\"ARG\"]\nextend-ignore = [\"TC002\"]\n\
             [lint.per-file-ignores]\n\"tests/**\" = [\"ARG\"]\n",
    );
    let result = import(&dir.path().join("svc")).unwrap();
    assert_eq!(result.absorb, Absorb::Full, "{:?}", result.absorb);
    let mut doc = DocumentMut::new();
    super::super::apply(&mut doc, &result.fragments);
    let doc: toml::Table = toml::from_str(&doc.to_string()).unwrap();
    let ruff = &doc["lint"]["python"]["ruff"];
    assert_eq!(ruff["line_length"].as_integer(), Some(120), "own setting wins");
    let select: Vec<&str> = ruff["select"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(
        select,
        ["E", "B", "ARG"],
        "the base select plus both extend-select lists"
    );
    let ignore: Vec<&str> = ruff["ignore"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(ignore, ["E501", "TC002"]);
    assert!(
        doc["per-file-ignores"].get("**/a.py").is_some(),
        "the base per-file-ignores are inherited"
    );
    assert!(doc["per-file-ignores"].get("tests/**").is_some());
    assert!(
        result.notes.iter().any(|note| note.contains("extend")),
        "{:?}",
        result.notes
    );
}

#[test]
fn an_extend_target_that_cannot_be_read_keeps_the_source() {
    let dir = tempdir().unwrap();
    write(dir.path(), "ruff.toml", "extend = \"../gone.toml\"\nline-length = 90\n");
    let result = import(dir.path()).unwrap();
    assert!(
        matches!(result.absorb, Absorb::Partial(ref keys) if keys[0].starts_with("extend =")),
        "{:?}",
        result.absorb
    );
}

#[test]
fn import_conventions_keys_are_carried() {
    let (result, doc) = migrate_pyproject(
        "[tool.ruff.lint.flake8-import-conventions]\nbanned-from = [\"itertools\"]\n\
             extend-aliases = { \"dask.dataframe\" = \"dd\" }\nbanned-aliases = { numpy = [\"npy\"] }\n",
    );
    assert_eq!(result.absorb, Absorb::Full);
    let ruff = &doc["lint"]["python"]["ruff"];
    assert_eq!(
        ruff["flake8_import_conventions_banned_from"][0].as_str(),
        Some("itertools")
    );
    assert_eq!(
        ruff["flake8_import_conventions_extend_aliases"]["dask.dataframe"].as_str(),
        Some("dd")
    );
    assert_eq!(
        ruff["flake8_import_conventions_banned_aliases"]["numpy"][0].as_str(),
        Some("npy")
    );
}

#[test]
fn every_isort_key_poly_honors_is_carried_including_required_imports() {
    let (result, doc) = migrate_pyproject(
        r#"
[tool.ruff.lint.isort]
case-sensitive = true
default-section = "first-party"
extra-standard-library = ["foo"]
force-wrap-aliases = true
from-first = true
lines-between-types = 1
no-lines-before = ["third-party"]
relative-imports-order = "closest-to-furthest"
section-order = ["future", "standard-library", "myco"]
split-on-trailing-comma = false
known-section = { myco = ["zzz", "yyy"] }
import-heading = { standard-library = "Stdlib" }
required-imports = ["from __future__ import annotations"]
"#,
    );
    assert_eq!(result.absorb, Absorb::Full);
    let ruff = &doc["lint"]["python"]["ruff"];
    assert_eq!(
        ruff["isort_required_imports"][0].as_str(),
        Some("from __future__ import annotations")
    );
    assert_eq!(ruff["isort_case_sensitive"].as_bool(), Some(true));
    assert_eq!(ruff["isort_default_section"].as_str(), Some("first-party"));
    assert_eq!(ruff["extra_standard_library"][0].as_str(), Some("foo"));
    assert_eq!(ruff["isort_lines_between_types"].as_integer(), Some(1));
    assert_eq!(ruff["isort_split_on_trailing_comma"].as_bool(), Some(false));
    assert_eq!(ruff["known_section"]["myco"][1].as_str(), Some("yyy"));
    assert_eq!(
        ruff["isort_import_heading"]["standard-library"].as_str(),
        Some("Stdlib")
    );
    assert_eq!(ruff["isort_section_order"].as_array().unwrap().len(), 3);
}

#[test]
fn lint_preview_is_carried_and_top_level_preview_keeps_the_source() {
    let (nested, doc) = migrate_pyproject("[tool.ruff.lint]\npreview = true\nexplicit-preview-rules = true\n");
    assert_eq!(nested.absorb, Absorb::Full);
    assert_eq!(doc["lint"]["python"]["ruff"]["preview"].as_bool(), Some(true));
    assert_eq!(
        doc["lint"]["python"]["ruff"]["explicit_preview_rules"].as_bool(),
        Some(true)
    );
    let (top, _) = migrate_pyproject("[tool.ruff]\npreview = true\n");
    assert_eq!(top.absorb, Absorb::Partial(vec!["preview".to_string()]));
}

#[test]
fn unsafe_fixes_is_carried_from_either_location() {
    let (_, top) = migrate_pyproject("[tool.ruff]\nunsafe-fixes = true\n");
    assert_eq!(top["lint"]["python"]["ruff"]["unsafe_fixes"].as_bool(), Some(true));
    let (_, nested) = migrate_pyproject("[tool.ruff.lint]\nunsafe-fixes = true\n");
    assert_eq!(nested["lint"]["python"]["ruff"]["unsafe_fixes"].as_bool(), Some(true));
}

const ARMIS_SHAPE: &str = r#"
[tool.ruff]
target-version = "py311"
line-length = 120
format.docstring-code-format = true
format.docstring-code-line-length = "dynamic"
format.quote-style = "double"
format.skip-magic-trailing-comma = false
lint.select = ["E", "F"]
lint.fixable = ["ALL"]
lint.unfixable = ["T201"]
lint.external = ["WPS", "E203"]
lint.dummy-variable-rgx = "^(_+|dummy)$"
lint.pycodestyle = { max-line-length = 100, ignore-overlong-task-comments = true }
lint.pep8-naming = { extend-ignore-names = ["db"], classmethod-decorators = ["classmethod"] }
lint.flake8-quotes = { inline-quotes = "single", avoid-escape = true }
lint.flake8-type-checking = { strict = false, runtime-evaluated-base-classes = ["pydantic.BaseModel"] }
lint.flake8-pytest-style = { fixture-parentheses = false, parametrize-names-type = "tuple" }
lint.isort = { known-first-party = ["acme"], known-third-party = ["requests"] }
"#;

fn migrate_pyproject(content: &str) -> (ImportResult, toml::Table) {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), content).unwrap();
    let result = import(dir.path()).expect("ruff config present");
    let mut doc = DocumentMut::new();
    super::super::apply(&mut doc, &result.fragments);
    (result, toml::from_str(&doc.to_string()).unwrap())
}

#[test]
fn plugin_and_fix_policy_keys_are_carried_and_the_source_is_fully_absorbed() {
    let (result, doc) = migrate_pyproject(ARMIS_SHAPE);
    assert_eq!(result.absorb, Absorb::Full, "notes: {:?}", result.notes);
    let ruff = &doc["lint"]["python"]["ruff"];
    assert_eq!(ruff["fixable"].as_array().unwrap().len(), 1);
    assert_eq!(ruff["unfixable"][0].as_str(), Some("T201"));
    assert_eq!(ruff["external"].as_array().unwrap().len(), 2);
    assert_eq!(ruff["dummy_variable_rgx"].as_str(), Some("^(_+|dummy)$"));
    assert_eq!(ruff["pycodestyle_max_line_length"].as_integer(), Some(100));
    assert_eq!(ruff["pycodestyle_ignore_overlong_task_comments"].as_bool(), Some(true));
    assert_eq!(ruff["pep8_naming_extend_ignore_names"][0].as_str(), Some("db"));
    assert_eq!(
        ruff["pep8_naming_classmethod_decorators"][0].as_str(),
        Some("classmethod")
    );
    assert_eq!(ruff["flake8_quotes_inline_quotes"].as_str(), Some("single"));
    assert_eq!(ruff["flake8_quotes_avoid_escape"].as_bool(), Some(true));
    assert_eq!(ruff["flake8_type_checking_strict"].as_bool(), Some(false));
    assert_eq!(
        ruff["flake8_type_checking_runtime_evaluated_base_classes"][0].as_str(),
        Some("pydantic.BaseModel")
    );
    assert_eq!(ruff["flake8_pytest_style_fixture_parentheses"].as_bool(), Some(false));
    assert_eq!(
        ruff["flake8_pytest_style_parametrize_names_type"].as_str(),
        Some("tuple")
    );
    assert_eq!(ruff["known_first_party"][0].as_str(), Some("acme"));
    assert_eq!(ruff["known_third_party"][0].as_str(), Some("requests"));
}

#[test]
fn line_length_and_docstring_code_settings_reach_the_formatter() {
    let (_, doc) = migrate_pyproject(ARMIS_SHAPE);
    let fmt = &doc["fmt"]["python"]["ruff"];
    assert_eq!(fmt["line_length"].as_integer(), Some(120));
    assert_eq!(fmt["docstring_code_format"].as_bool(), Some(true));
    assert_eq!(fmt["docstring_code_line_length"].as_str(), Some("dynamic"));
    assert_eq!(fmt["quote_style"].as_str(), Some("double"));
    assert_eq!(fmt["skip_magic_trailing_comma"].as_bool(), Some(false));
}

#[test]
fn plugin_settings_poly_cannot_honor_are_reported_by_name() {
    let (result, _) = migrate_pyproject(
        r#"
[tool.ruff.lint]
select = ["E"]
isort = { known-first-party = ["acme"], force-single-line = true, lines-after-imports = 2 }
pylint = { max-args = 8, max-locals = 20, allow-magic-value-types = ["str"] }
pydocstyle = { convention = "google", ignore-decorators = ["typing.overload"], unknown-key = true }
"#,
    );
    let Absorb::Partial(keys) = result.absorb else {
        panic!("unsupported plugin keys must keep the source");
    };
    assert_eq!(
        keys,
        ["lint.pydocstyle.unknown-key", "lint.pylint.allow-magic-value-types"]
    );
}

#[test]
fn a_value_of_the_wrong_type_is_reported_not_carried() {
    let (result, doc) = migrate_pyproject("[tool.ruff.lint]\nfixable = \"ALL\"\n");
    assert_eq!(
        result.absorb,
        Absorb::Partial(vec!["lint.fixable (unexpected value type)".to_string()])
    );
    assert!(doc["lint"]["python"]["ruff"].get("fixable").is_none());
}

const EVERY_PLUGIN_TABLE: &str = r#"
[tool.ruff]
builtins = ["_"]
namespace-packages = ["pkg/ns"]

[tool.ruff.lint]
select = ["E"]
logger-objects = ["app.log"]
task-tags = ["MYTAG"]
typing-modules = ["mylib.typing"]
typing-extensions = false
future-annotations = true
allowed-confusables = ["a"]

[tool.ruff.lint.isort]
required-imports = ["from __future__ import annotations"]
sections = { myco = ["zzz"] }

[tool.ruff.lint.flake8-annotations]
mypy-init-return = true
suppress-none-returning = true

[tool.ruff.lint.flake8-bandit]
hardcoded-tmp-directory = ["/mnt/scratch"]
hardcoded-tmp-directory-extend = ["/srv/tmp"]
check-typed-exception = true

[tool.ruff.lint.flake8-bugbear]
extend-immutable-calls = ["lib.make"]

[tool.ruff.lint.flake8-builtins]
builtins-ignorelist = ["id"]
allowed-modules = ["types"]
strict-checking = true

[tool.ruff.lint.flake8-copyright]
notice-rgx = "Acme"
author = "Acme Corp"
min-file-size = 100

[tool.ruff.lint.flake8-errmsg]
max-string-length = 20

[tool.ruff.lint.flake8-gettext]
function-names = ["tr"]
extend-function-names = ["ngettext"]

[tool.ruff.lint.flake8-self]
extend-ignore-names = ["_secret"]

[tool.ruff.lint.flake8-tidy-imports]
ban-relative-imports = "all"
banned-module-level-imports = ["torch"]
require-lazy = { include = "all", exclude = ["typing"] }

[tool.ruff.lint.flake8-tidy-imports.banned-api]
"cgi".msg = "deprecated"

[tool.ruff.lint.pydocstyle]
ignore-decorators = ["typing.overload"]
property-decorators = ["lib.cached"]
ignore-var-parameters = true

[tool.ruff.lint.pyflakes]
allowed-unused-imports = ["hvplot.pandas"]

[tool.ruff.lint.pyupgrade]
keep-runtime-typing = true

[tool.ruff.lint.ruff]
parenthesize-tuple-in-subscript = true
"#;

#[test]
fn every_plugin_table_poly_honors_is_carried_and_the_source_is_fully_absorbed() {
    let (result, doc) = migrate_pyproject(EVERY_PLUGIN_TABLE);
    assert_eq!(result.absorb, Absorb::Full, "notes: {:?}", result.notes);
    let ruff = &doc["lint"]["python"]["ruff"];
    assert_eq!(ruff["builtins"][0].as_str(), Some("_"));
    assert_eq!(ruff["namespace_packages"][0].as_str(), Some("pkg/ns"));
    assert_eq!(ruff["logger_objects"][0].as_str(), Some("app.log"));
    assert_eq!(ruff["task_tags"][0].as_str(), Some("MYTAG"));
    assert_eq!(ruff["typing_modules"][0].as_str(), Some("mylib.typing"));
    assert_eq!(ruff["typing_extensions"].as_bool(), Some(false));
    assert_eq!(ruff["future_annotations"].as_bool(), Some(true));
    assert_eq!(ruff["allowed_confusables"][0].as_str(), Some("a"));
    assert_eq!(
        ruff["isort_required_imports"][0].as_str(),
        Some("from __future__ import annotations")
    );
    assert_eq!(ruff["known_section"]["myco"][0].as_str(), Some("zzz"));
    assert_eq!(ruff["flake8_annotations_mypy_init_return"].as_bool(), Some(true));
    assert_eq!(ruff["flake8_annotations_suppress_none_returning"].as_bool(), Some(true));
    assert_eq!(
        ruff["flake8_bandit_hardcoded_tmp_directory"][0].as_str(),
        Some("/mnt/scratch")
    );
    assert_eq!(
        ruff["flake8_bandit_hardcoded_tmp_directory_extend"][0].as_str(),
        Some("/srv/tmp")
    );
    assert_eq!(ruff["flake8_bandit_check_typed_exception"].as_bool(), Some(true));
    assert_eq!(
        ruff["flake8_bugbear_extend_immutable_calls"][0].as_str(),
        Some("lib.make")
    );
    assert_eq!(ruff["flake8_builtins_ignorelist"][0].as_str(), Some("id"));
    assert_eq!(ruff["flake8_builtins_allowed_modules"][0].as_str(), Some("types"));
    assert_eq!(ruff["flake8_builtins_strict_checking"].as_bool(), Some(true));
    assert_eq!(ruff["flake8_copyright_notice_rgx"].as_str(), Some("Acme"));
    assert_eq!(ruff["flake8_copyright_author"].as_str(), Some("Acme Corp"));
    assert_eq!(ruff["flake8_copyright_min_file_size"].as_integer(), Some(100));
    assert_eq!(ruff["flake8_errmsg_max_string_length"].as_integer(), Some(20));
    assert_eq!(ruff["flake8_gettext_function_names"][0].as_str(), Some("tr"));
    assert_eq!(
        ruff["flake8_gettext_extend_function_names"][0].as_str(),
        Some("ngettext")
    );
    assert_eq!(ruff["flake8_self_extend_ignore_names"][0].as_str(), Some("_secret"));
    assert_eq!(ruff["flake8_tidy_imports_ban_relative_imports"].as_str(), Some("all"));
    assert_eq!(
        ruff["flake8_tidy_imports_banned_module_level_imports"][0].as_str(),
        Some("torch")
    );
    assert_eq!(
        ruff["flake8_tidy_imports_banned_api"]["cgi"]["msg"].as_str(),
        Some("deprecated"),
        "a nested table survives"
    );
    assert_eq!(
        ruff["flake8_tidy_imports_require_lazy"]["include"].as_str(),
        Some("all")
    );
    assert_eq!(
        ruff["flake8_tidy_imports_require_lazy"]["exclude"][0].as_str(),
        Some("typing")
    );
    assert_eq!(
        ruff["pydocstyle_ignore_decorators"][0].as_str(),
        Some("typing.overload")
    );
    assert_eq!(ruff["pydocstyle_property_decorators"][0].as_str(), Some("lib.cached"));
    assert_eq!(ruff["pydocstyle_ignore_var_parameters"].as_bool(), Some(true));
    assert_eq!(
        ruff["pyflakes_allowed_unused_imports"][0].as_str(),
        Some("hvplot.pandas")
    );
    assert_eq!(ruff["pyupgrade_keep_runtime_typing"].as_bool(), Some(true));
    assert_eq!(ruff["ruff_parenthesize_tuple_in_subscript"].as_bool(), Some(true));
}

#[test]
fn a_plugin_key_ruff_does_not_define_is_still_reported() {
    let (result, _) = migrate_pyproject("[tool.ruff.lint.flake8-bugbear]\nmade-up = true\n");
    assert_eq!(
        result.absorb,
        Absorb::Partial(vec!["lint.flake8-bugbear.made-up".to_string()])
    );
}
