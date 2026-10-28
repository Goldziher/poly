//! The ruff keys the importer maps: which top-level and `[lint]` keys it honors,
//! and where each plugin, lint and format setting lands in `poly.toml`.

use toml_edit::Item;

use crate::migrate::importers::{bool_item, int_item, str_array, str_item};

/// Top-level `[tool.ruff]` / `ruff.toml` keys this importer fully honors.
///
/// `exclude` / `extend-exclude` fold into `[discovery] exclude`. `format`
/// (`[tool.ruff.format]`) is carried for the keys in [`FORMAT_KEYS`]; any other
/// key is reported as unrepresented.
pub(super) const RECOGNIZED_TOP: &[&str] = &[
    "lint",
    "select",
    "extend-select",
    "ignore",
    "per-file-ignores",
    "target-version",
    "target_version",
    "src",
    "line-length",
    "line_length",
    "unsafe-fixes",
    "indent-width",
    "exclude",
    "extend-exclude",
    "extend_exclude",
    "format",
    "extend",
    "builtins",
    "namespace-packages",
];

/// `[lint]`-subtable keys this importer honors, at least in part. Plugin tables
/// (`pylint`, `isort`, …) are only honored for the keys in [`PLUGIN_KEYS`]; any
/// other key inside them is reported as unrepresented.
pub(super) const RECOGNIZED_LINT: &[&str] = &[
    "select",
    "extend-select",
    "ignore",
    "extend-ignore",
    "per-file-ignores",
    "extend-per-file-ignores",
    "fixable",
    "unfixable",
    "external",
    "dummy-variable-rgx",
    "unsafe-fixes",
    "preview",
    "explicit-preview-rules",
    "pydocstyle",
    "mccabe",
    "pylint",
    "isort",
    "pycodestyle",
    "pep8-naming",
    "flake8-import-conventions",
    "flake8-quotes",
    "flake8-type-checking",
    "flake8-pytest-style",
    "flake8-annotations",
    "flake8-bandit",
    "flake8-boolean-trap",
    "flake8-bugbear",
    "flake8-builtins",
    "flake8-comprehensions",
    "flake8-copyright",
    "flake8-errmsg",
    "flake8-gettext",
    "flake8-implicit-str-concat",
    "flake8-self",
    "flake8-tidy-imports",
    "flake8-unused-arguments",
    "pydoclint",
    "pyflakes",
    "pyupgrade",
    "ruff",
    "allowed-confusables",
    "logger-objects",
    "task-tags",
    "typing-modules",
    "typing-extensions",
    "future-annotations",
    "ignore-init-module-imports",
];

/// How a plugin key's value is carried across.
#[derive(Clone, Copy)]
pub(super) enum Kind {
    Str,
    Int,
    IntOrStr,
    Bool,
    List,
    /// A table whose values are strings or lists of strings.
    Table,
    /// Any TOML value built from strings, integers, booleans, lists and tables
    /// (`banned-api`'s nested `{ msg }` tables, `require-lazy`'s three shapes).
    Any,
}

/// `(ruff plugin table, ruff key, poly key, kind)` for every plugin setting the
/// poly ruff engine reads (`engines/ruff.rs`, `engines/ruff/plugins.rs`).
pub(super) const PLUGIN_KEYS: &[(&str, &str, &str, Kind)] = &[
    ("pydocstyle", "convention", "pydocstyle_convention", Kind::Str),
    ("mccabe", "max-complexity", "mccabe_max_complexity", Kind::Int),
    ("pylint", "max-args", "pylint_max_args", Kind::Int),
    ("pylint", "max-bool-expr", "pylint_max_bool_expr", Kind::Int),
    ("pylint", "max-branches", "pylint_max_branches", Kind::Int),
    ("pylint", "max-locals", "pylint_max_locals", Kind::Int),
    ("pylint", "max-nested-blocks", "pylint_max_nested_blocks", Kind::Int),
    ("pylint", "max-public-methods", "pylint_max_public_methods", Kind::Int),
    ("pylint", "max-returns", "pylint_max_returns", Kind::Int),
    ("pylint", "max-statements", "pylint_max_statements", Kind::Int),
    ("isort", "known-first-party", "known_first_party", Kind::List),
    ("isort", "known-third-party", "known_third_party", Kind::List),
    ("isort", "known-local-folder", "known_local_folder", Kind::List),
    ("isort", "combine-as-imports", "isort_combine_as_imports", Kind::Bool),
    ("isort", "force-single-line", "isort_force_single_line", Kind::Bool),
    (
        "isort",
        "force-sort-within-sections",
        "isort_force_sort_within_sections",
        Kind::Bool,
    ),
    ("isort", "lines-after-imports", "isort_lines_after_imports", Kind::Int),
    ("isort", "order-by-type", "isort_order_by_type", Kind::Bool),
    ("isort", "case-sensitive", "isort_case_sensitive", Kind::Bool),
    ("isort", "classes", "isort_classes", Kind::List),
    ("isort", "constants", "isort_constants", Kind::List),
    ("isort", "variables", "isort_variables", Kind::List),
    ("isort", "default-section", "isort_default_section", Kind::Str),
    ("isort", "detect-same-package", "isort_detect_same_package", Kind::Bool),
    ("isort", "extra-standard-library", "extra_standard_library", Kind::List),
    ("isort", "force-to-top", "isort_force_to_top", Kind::List),
    ("isort", "force-wrap-aliases", "isort_force_wrap_aliases", Kind::Bool),
    ("isort", "forced-separate", "isort_forced_separate", Kind::List),
    ("isort", "from-first", "isort_from_first", Kind::Bool),
    ("isort", "import-heading", "isort_import_heading", Kind::Table),
    ("isort", "known-section", "known_section", Kind::Table),
    ("isort", "length-sort", "isort_length_sort", Kind::Bool),
    (
        "isort",
        "length-sort-straight",
        "isort_length_sort_straight",
        Kind::Bool,
    ),
    ("isort", "lines-between-types", "isort_lines_between_types", Kind::Int),
    ("isort", "no-lines-before", "isort_no_lines_before", Kind::List),
    ("isort", "no-sections", "isort_no_sections", Kind::Bool),
    (
        "isort",
        "relative-imports-order",
        "isort_relative_imports_order",
        Kind::Str,
    ),
    ("isort", "section-order", "isort_section_order", Kind::List),
    (
        "isort",
        "single-line-exclusions",
        "isort_single_line_exclusions",
        Kind::List,
    ),
    (
        "isort",
        "split-on-trailing-comma",
        "isort_split_on_trailing_comma",
        Kind::Bool,
    ),
    (
        "pycodestyle",
        "max-line-length",
        "pycodestyle_max_line_length",
        Kind::Int,
    ),
    ("pycodestyle", "max-doc-length", "pycodestyle_max_doc_length", Kind::Int),
    (
        "pycodestyle",
        "ignore-overlong-task-comments",
        "pycodestyle_ignore_overlong_task_comments",
        Kind::Bool,
    ),
    ("pep8-naming", "ignore-names", "pep8_naming_ignore_names", Kind::List),
    (
        "pep8-naming",
        "extend-ignore-names",
        "pep8_naming_extend_ignore_names",
        Kind::List,
    ),
    (
        "pep8-naming",
        "classmethod-decorators",
        "pep8_naming_classmethod_decorators",
        Kind::List,
    ),
    (
        "pep8-naming",
        "staticmethod-decorators",
        "pep8_naming_staticmethod_decorators",
        Kind::List,
    ),
    (
        "flake8-import-conventions",
        "aliases",
        "flake8_import_conventions_aliases",
        Kind::Table,
    ),
    (
        "flake8-import-conventions",
        "extend-aliases",
        "flake8_import_conventions_extend_aliases",
        Kind::Table,
    ),
    (
        "flake8-import-conventions",
        "banned-aliases",
        "flake8_import_conventions_banned_aliases",
        Kind::Table,
    ),
    (
        "flake8-import-conventions",
        "banned-from",
        "flake8_import_conventions_banned_from",
        Kind::List,
    ),
    (
        "flake8-quotes",
        "inline-quotes",
        "flake8_quotes_inline_quotes",
        Kind::Str,
    ),
    (
        "flake8-quotes",
        "multiline-quotes",
        "flake8_quotes_multiline_quotes",
        Kind::Str,
    ),
    (
        "flake8-quotes",
        "docstring-quotes",
        "flake8_quotes_docstring_quotes",
        Kind::Str,
    ),
    (
        "flake8-quotes",
        "avoid-escape",
        "flake8_quotes_avoid_escape",
        Kind::Bool,
    ),
    (
        "flake8-type-checking",
        "strict",
        "flake8_type_checking_strict",
        Kind::Bool,
    ),
    (
        "flake8-type-checking",
        "quote-annotations",
        "flake8_type_checking_quote_annotations",
        Kind::Bool,
    ),
    (
        "flake8-type-checking",
        "exempt-modules",
        "flake8_type_checking_exempt_modules",
        Kind::List,
    ),
    (
        "flake8-type-checking",
        "runtime-evaluated-base-classes",
        "flake8_type_checking_runtime_evaluated_base_classes",
        Kind::List,
    ),
    (
        "flake8-type-checking",
        "runtime-evaluated-decorators",
        "flake8_type_checking_runtime_evaluated_decorators",
        Kind::List,
    ),
    (
        "flake8-pytest-style",
        "fixture-parentheses",
        "flake8_pytest_style_fixture_parentheses",
        Kind::Bool,
    ),
    (
        "flake8-pytest-style",
        "mark-parentheses",
        "flake8_pytest_style_mark_parentheses",
        Kind::Bool,
    ),
    (
        "flake8-pytest-style",
        "parametrize-names-type",
        "flake8_pytest_style_parametrize_names_type",
        Kind::Str,
    ),
    (
        "flake8-pytest-style",
        "parametrize-values-type",
        "flake8_pytest_style_parametrize_values_type",
        Kind::Str,
    ),
    (
        "flake8-pytest-style",
        "parametrize-values-row-type",
        "flake8_pytest_style_parametrize_values_row_type",
        Kind::Str,
    ),
    ("isort", "required-imports", "isort_required_imports", Kind::List),
    ("isort", "sections", "known_section", Kind::Table),
    (
        "flake8-annotations",
        "mypy-init-return",
        "flake8_annotations_mypy_init_return",
        Kind::Bool,
    ),
    (
        "flake8-annotations",
        "suppress-dummy-args",
        "flake8_annotations_suppress_dummy_args",
        Kind::Bool,
    ),
    (
        "flake8-annotations",
        "suppress-none-returning",
        "flake8_annotations_suppress_none_returning",
        Kind::Bool,
    ),
    (
        "flake8-annotations",
        "allow-star-arg-any",
        "flake8_annotations_allow_star_arg_any",
        Kind::Bool,
    ),
    (
        "flake8-annotations",
        "ignore-fully-untyped",
        "flake8_annotations_ignore_fully_untyped",
        Kind::Bool,
    ),
    (
        "flake8-bandit",
        "hardcoded-tmp-directory",
        "flake8_bandit_hardcoded_tmp_directory",
        Kind::List,
    ),
    (
        "flake8-bandit",
        "hardcoded-tmp-directory-extend",
        "flake8_bandit_hardcoded_tmp_directory_extend",
        Kind::List,
    ),
    (
        "flake8-bandit",
        "check-typed-exception",
        "flake8_bandit_check_typed_exception",
        Kind::Bool,
    ),
    (
        "flake8-bandit",
        "extend-markup-names",
        "flake8_bandit_extend_markup_names",
        Kind::List,
    ),
    (
        "flake8-bandit",
        "allowed-markup-calls",
        "flake8_bandit_allowed_markup_calls",
        Kind::List,
    ),
    (
        "flake8-boolean-trap",
        "extend-allowed-calls",
        "flake8_boolean_trap_extend_allowed_calls",
        Kind::List,
    ),
    (
        "flake8-bugbear",
        "extend-immutable-calls",
        "flake8_bugbear_extend_immutable_calls",
        Kind::List,
    ),
    (
        "flake8-builtins",
        "ignorelist",
        "flake8_builtins_ignorelist",
        Kind::List,
    ),
    (
        "flake8-builtins",
        "builtins-ignorelist",
        "flake8_builtins_ignorelist",
        Kind::List,
    ),
    (
        "flake8-builtins",
        "allowed-modules",
        "flake8_builtins_allowed_modules",
        Kind::List,
    ),
    (
        "flake8-builtins",
        "builtins-allowed-modules",
        "flake8_builtins_allowed_modules",
        Kind::List,
    ),
    (
        "flake8-builtins",
        "strict-checking",
        "flake8_builtins_strict_checking",
        Kind::Bool,
    ),
    (
        "flake8-builtins",
        "builtins-strict-checking",
        "flake8_builtins_strict_checking",
        Kind::Bool,
    ),
    (
        "flake8-comprehensions",
        "allow-dict-calls-with-keyword-arguments",
        "flake8_comprehensions_allow_dict_calls_with_keyword_arguments",
        Kind::Bool,
    ),
    (
        "flake8-copyright",
        "notice-rgx",
        "flake8_copyright_notice_rgx",
        Kind::Str,
    ),
    ("flake8-copyright", "author", "flake8_copyright_author", Kind::Str),
    (
        "flake8-copyright",
        "min-file-size",
        "flake8_copyright_min_file_size",
        Kind::Int,
    ),
    (
        "flake8-errmsg",
        "max-string-length",
        "flake8_errmsg_max_string_length",
        Kind::Int,
    ),
    (
        "flake8-gettext",
        "function-names",
        "flake8_gettext_function_names",
        Kind::List,
    ),
    (
        "flake8-gettext",
        "extend-function-names",
        "flake8_gettext_extend_function_names",
        Kind::List,
    ),
    (
        "flake8-implicit-str-concat",
        "allow-multiline",
        "flake8_implicit_str_concat_allow_multiline",
        Kind::Bool,
    ),
    ("flake8-self", "ignore-names", "flake8_self_ignore_names", Kind::List),
    (
        "flake8-self",
        "extend-ignore-names",
        "flake8_self_extend_ignore_names",
        Kind::List,
    ),
    (
        "flake8-tidy-imports",
        "ban-relative-imports",
        "flake8_tidy_imports_ban_relative_imports",
        Kind::Str,
    ),
    (
        "flake8-tidy-imports",
        "banned-api",
        "flake8_tidy_imports_banned_api",
        Kind::Any,
    ),
    (
        "flake8-tidy-imports",
        "extend-banned-api",
        "flake8_tidy_imports_extend_banned_api",
        Kind::Any,
    ),
    (
        "flake8-tidy-imports",
        "banned-module-level-imports",
        "flake8_tidy_imports_banned_module_level_imports",
        Kind::List,
    ),
    (
        "flake8-tidy-imports",
        "require-lazy",
        "flake8_tidy_imports_require_lazy",
        Kind::Any,
    ),
    (
        "flake8-tidy-imports",
        "ban-lazy",
        "flake8_tidy_imports_ban_lazy",
        Kind::Any,
    ),
    (
        "flake8-unused-arguments",
        "ignore-variadic-names",
        "flake8_unused_arguments_ignore_variadic_names",
        Kind::Bool,
    ),
    (
        "pydocstyle",
        "ignore-decorators",
        "pydocstyle_ignore_decorators",
        Kind::List,
    ),
    (
        "pydocstyle",
        "property-decorators",
        "pydocstyle_property_decorators",
        Kind::List,
    ),
    (
        "pydocstyle",
        "ignore-var-parameters",
        "pydocstyle_ignore_var_parameters",
        Kind::Bool,
    ),
    (
        "pydoclint",
        "ignore-one-line-docstrings",
        "pydoclint_ignore_one_line_docstrings",
        Kind::Bool,
    ),
    ("pyflakes", "extend-generics", "pyflakes_extend_generics", Kind::List),
    (
        "pyflakes",
        "allowed-unused-imports",
        "pyflakes_allowed_unused_imports",
        Kind::List,
    ),
    (
        "pyupgrade",
        "keep-runtime-typing",
        "pyupgrade_keep_runtime_typing",
        Kind::Bool,
    ),
    (
        "ruff",
        "parenthesize-tuple-in-subscript",
        "ruff_parenthesize_tuple_in_subscript",
        Kind::Bool,
    ),
    (
        "ruff",
        "strictly-empty-init-modules",
        "ruff_strictly_empty_init_modules",
        Kind::Bool,
    ),
];

/// Top-level `[lint]` settings carried as-is: `(ruff key, poly key, kind)`.
pub(super) const LINT_KEYS: &[(&str, &str, Kind)] = &[
    ("fixable", "fixable", Kind::List),
    ("unfixable", "unfixable", Kind::List),
    ("external", "external", Kind::List),
    ("dummy-variable-rgx", "dummy_variable_rgx", Kind::Str),
    ("unsafe-fixes", "unsafe_fixes", Kind::Bool),
    ("explicit-preview-rules", "explicit_preview_rules", Kind::Bool),
    ("builtins", "builtins", Kind::List),
    ("namespace-packages", "namespace_packages", Kind::List),
    ("allowed-confusables", "allowed_confusables", Kind::List),
    ("logger-objects", "logger_objects", Kind::List),
    ("task-tags", "task_tags", Kind::List),
    ("typing-modules", "typing_modules", Kind::List),
    ("typing-extensions", "typing_extensions", Kind::Bool),
    ("future-annotations", "future_annotations", Kind::Bool),
    ("ignore-init-module-imports", "ignore_init_module_imports", Kind::Bool),
];

/// `[format]` keys the poly ruff formatter reads: `(ruff key, poly key, kind)`.
pub(super) const FORMAT_KEYS: &[(&str, &str, Kind)] = &[
    ("docstring-code-format", "docstring_code_format", Kind::Bool),
    (
        "docstring-code-line-length",
        "docstring_code_line_length",
        Kind::IntOrStr,
    ),
    ("indent-style", "indent_style", Kind::Str),
    ("line-ending", "line_ending", Kind::Str),
    ("quote-style", "quote_style", Kind::Str),
    ("skip-magic-trailing-comma", "skip_magic_trailing_comma", Kind::Bool),
    ("preview", "preview", Kind::Bool),
    ("exclude", "exclude", Kind::List),
];

/// A table of strings and string lists as an inline table; `None` when any value
/// is something else.
fn table_item(table: &toml::Table) -> Option<Item> {
    let mut inline = toml_edit::InlineTable::new();
    for (key, value) in table {
        let leaf = match value {
            toml::Value::String(text) => toml_edit::Value::from(text.as_str()),
            toml::Value::Array(_) => {
                let items: Option<Vec<String>> = value
                    .as_array()?
                    .iter()
                    .map(|v| v.as_str().map(str::to_string))
                    .collect();
                let Item::Value(array) = str_array(&items?) else {
                    return None;
                };
                array
            }
            _ => return None,
        };
        inline.insert(key, leaf);
    }
    Some(Item::Value(toml_edit::Value::InlineTable(inline)))
}

/// A string, integer, boolean, list or table of those as a TOML value; `None`
/// when anything else (a float, a datetime) is inside.
fn value_item(value: &toml::Value) -> Option<toml_edit::Value> {
    Some(match value {
        toml::Value::String(text) => toml_edit::Value::from(text.as_str()),
        toml::Value::Integer(number) => toml_edit::Value::from(*number),
        toml::Value::Boolean(flag) => toml_edit::Value::from(*flag),
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(value_item(item)?);
            }
            toml_edit::Value::Array(array)
        }
        toml::Value::Table(table) => {
            let mut inline = toml_edit::InlineTable::new();
            for (key, inner) in table {
                inline.insert(key, value_item(inner)?);
            }
            toml_edit::Value::InlineTable(inline)
        }
        _ => return None,
    })
}

pub(super) fn carry(value: &toml::Value, kind: Kind) -> Option<Item> {
    match kind {
        Kind::Str => value.as_str().map(str_item),
        Kind::Int => value.as_integer().map(int_item),
        Kind::IntOrStr => value
            .as_integer()
            .map(int_item)
            .or_else(|| value.as_str().map(str_item)),
        Kind::Bool => value.as_bool().map(bool_item),
        Kind::Table => table_item(value.as_table()?),
        Kind::Any => value_item(value).map(Item::Value),
        Kind::List => {
            let items: Option<Vec<String>> = value
                .as_array()?
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect();
            items.map(|items| str_array(&items))
        }
    }
}
