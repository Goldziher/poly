//! Ruff config importer.
//!
//! Reads `ruff.toml` / `.ruff.toml` (flat or `[lint]`-nested) or, failing that,
//! `pyproject.toml` `[tool.ruff]`. Ports the selection vocabulary and the plugin
//! knobs the poly ruff engine understands into `[lint.python.ruff]`, and
//! ruff's `per-file-ignores` into the top-level `[per-file-ignores]` table.

use std::path::Path;

use toml_edit::Item;

use super::{Absorb, Fragment, ImportResult, bool_item, int_item, str_array, str_item, toml_string_list};

/// Top-level `[tool.ruff]` / `ruff.toml` keys this importer fully honors.
///
/// `exclude` / `extend-exclude` fold into `[discovery] exclude`. `format`
/// (`[tool.ruff.format]`) is intentionally dropped: poly formats Python with
/// opinionated ruff-formatter defaults and does not read ruff's format options,
/// so keeping the section would change nothing — it is recorded as a note.
const RECOGNIZED_TOP: &[&str] = &[
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
    "indent-width",
    "exclude",
    "extend-exclude",
    "extend_exclude",
    "format",
];

/// `[lint]`-subtable keys this importer honors, at least in part. Plugin tables
/// (`pylint`, `isort`, …) are only honored for the keys in [`PLUGIN_KEYS`]; any
/// other key inside them is reported as unrepresented.
const RECOGNIZED_LINT: &[&str] = &[
    "select",
    "extend-select",
    "ignore",
    "per-file-ignores",
    "fixable",
    "unfixable",
    "external",
    "dummy-variable-rgx",
    "pydocstyle",
    "mccabe",
    "pylint",
    "isort",
    "pycodestyle",
    "pep8-naming",
    "flake8-quotes",
    "flake8-type-checking",
    "flake8-pytest-style",
];

/// How a plugin key's value is carried across.
#[derive(Clone, Copy)]
enum Kind {
    Str,
    Int,
    IntOrStr,
    Bool,
    List,
}

/// `(ruff plugin table, ruff key, poly key, kind)` for every plugin setting the
/// poly ruff engine reads (`engines/ruff.rs`, `engines/ruff/plugins.rs`).
const PLUGIN_KEYS: &[(&str, &str, &str, Kind)] = &[
    ("pydocstyle", "convention", "pydocstyle_convention", Kind::Str),
    ("mccabe", "max-complexity", "mccabe_max_complexity", Kind::Int),
    ("pylint", "max-args", "pylint_max_args", Kind::Int),
    ("pylint", "max-branches", "pylint_max_branches", Kind::Int),
    ("pylint", "max-returns", "pylint_max_returns", Kind::Int),
    ("isort", "known-first-party", "known_first_party", Kind::List),
    ("isort", "known-third-party", "known_third_party", Kind::List),
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
];

/// Top-level `[lint]` settings carried as-is: `(ruff key, poly key, kind)`.
const LINT_KEYS: &[(&str, &str, Kind)] = &[
    ("fixable", "fixable", Kind::List),
    ("unfixable", "unfixable", Kind::List),
    ("external", "external", Kind::List),
    ("dummy-variable-rgx", "dummy_variable_rgx", Kind::Str),
];

/// `[format]` keys the poly ruff formatter reads: `(ruff key, poly key, kind)`.
const FORMAT_KEYS: &[(&str, &str, Kind)] = &[
    ("docstring-code-format", "docstring_code_format", Kind::Bool),
    (
        "docstring-code-line-length",
        "docstring_code_line_length",
        Kind::IntOrStr,
    ),
];

/// Run the ruff importer against `dir`.
pub fn import(dir: &Path) -> Option<ImportResult> {
    let (table, source) = load(dir)?;
    let lint = table.get("lint").and_then(toml::Value::as_table);
    let pick = |key: &str| lint.and_then(|l| l.get(key)).or_else(|| table.get(key));

    let mut ruff_entries: Vec<(String, Item)> = Vec::new();

    if let Some(version) = table
        .get("target-version")
        .or_else(|| table.get("target_version"))
        .and_then(toml::Value::as_str)
    {
        ruff_entries.push(("target_version".to_string(), str_item(version)));
    }
    let src = toml_string_list(table.get("src"));
    if !src.is_empty() {
        ruff_entries.push(("src".to_string(), str_array(&src)));
    }
    let mut leftovers = Vec::new();
    for (plugin, key, poly_key, kind) in PLUGIN_KEYS {
        let value = lint
            .and_then(|l| l.get(*plugin))
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(*key));
        push_carried(
            &mut ruff_entries,
            &mut leftovers,
            &format!("lint.{plugin}.{key}"),
            poly_key,
            value,
            *kind,
        );
    }
    if let Some(value) = table
        .get("line-length")
        .or_else(|| table.get("line_length"))
        .and_then(toml::Value::as_integer)
    {
        ruff_entries.push(("line_length".to_string(), int_item(value)));
    }

    let mut select = toml_string_list(pick("select"));
    select.extend(toml_string_list(pick("extend-select")));
    if !select.is_empty() {
        ruff_entries.push(("select".to_string(), str_array(&select)));
    }
    let ignore = toml_string_list(pick("ignore"));
    if !ignore.is_empty() {
        ruff_entries.push(("ignore".to_string(), str_array(&ignore)));
    }

    for (key, poly_key, kind) in LINT_KEYS {
        push_carried(
            &mut ruff_entries,
            &mut leftovers,
            &format!("lint.{key}"),
            poly_key,
            pick(key),
            *kind,
        );
    }

    let per_file = collect_per_file(pick("per-file-ignores"));

    let mut excludes = toml_string_list(table.get("extend-exclude"));
    excludes.extend(toml_string_list(table.get("extend_exclude")));
    excludes.extend(toml_string_list(table.get("exclude")));
    excludes.sort();
    excludes.dedup();

    let mut fmt_entries: Vec<(String, Item)> = Vec::new();
    if let Some(value) = table
        .get("line-length")
        .or_else(|| table.get("line_length"))
        .and_then(toml::Value::as_integer)
    {
        fmt_entries.push(("line_length".to_string(), int_item(value)));
    }
    if let Some(value) = table.get("indent-width").and_then(toml::Value::as_integer) {
        fmt_entries.push(("indent_width".to_string(), int_item(value)));
    }
    let mut notes = Vec::new();
    if let Some(format) = table.get("format").and_then(toml::Value::as_table) {
        let mut dropped = Vec::new();
        for (key, value) in format {
            match FORMAT_KEYS.iter().find(|(ruff_key, _, _)| ruff_key == key) {
                Some((_, poly_key, kind)) => {
                    let before = fmt_entries.len();
                    push_carried(
                        &mut fmt_entries,
                        &mut leftovers,
                        &format!("format.{key}"),
                        poly_key,
                        Some(value),
                        *kind,
                    );
                    if fmt_entries.len() == before {
                        dropped.push(key.clone());
                    }
                }
                None => dropped.push(key.clone()),
            }
        }
        if !dropped.is_empty() {
            notes.push(format!(
                "dropped [tool.ruff.format] keys poly's formatter does not read: {}",
                dropped.join(", ")
            ));
        }
    }

    let mut fragments = Vec::new();
    if !ruff_entries.is_empty() {
        fragments.push(Fragment::new(&["lint", "python", "ruff"], ruff_entries));
    }
    if !fmt_entries.is_empty() {
        fragments.push(Fragment::new(&["fmt", "python", "ruff"], fmt_entries));
    }
    if !per_file.is_empty() {
        fragments.push(Fragment::new(&["per-file-ignores"], per_file));
    }
    if !excludes.is_empty() {
        fragments.push(Fragment::new(
            &["discovery"],
            vec![("exclude".to_string(), str_array(&excludes))],
        ));
    }

    leftovers.extend(leftover_keys(&table, lint));
    leftovers.sort();
    leftovers.dedup();
    let absorb = if fragments.is_empty() && leftovers.is_empty() {
        Absorb::None
    } else if leftovers.is_empty() {
        Absorb::Full
    } else {
        Absorb::Partial(leftovers)
    };

    Some(ImportResult {
        tool: "ruff",
        sources: vec![source],
        fragments,
        notes,
        absorb,
    })
}

/// Load the ruff config: `ruff.toml` / `.ruff.toml` first, then
/// `pyproject.toml` `[tool.ruff]`. Returns the ruff sub-table and the file it
/// came from.
fn load(dir: &Path) -> Option<(toml::Table, std::path::PathBuf)> {
    for name in ["ruff.toml", ".ruff.toml"] {
        let path = dir.join(name);
        if let Some(table) = super::load_toml(&path) {
            return Some((table, path));
        }
    }
    let pyproject = dir.join("pyproject.toml");
    let table = super::load_toml(&pyproject)?;
    let ruff = table
        .get("tool")
        .and_then(toml::Value::as_table)
        .and_then(|tool| tool.get("ruff"))
        .and_then(toml::Value::as_table)?
        .clone();
    Some((ruff, pyproject))
}

/// Collect `per-file-ignores` into ordered `"glob" = [codes]` entries.
fn collect_per_file(value: Option<&toml::Value>) -> Vec<(String, Item)> {
    let Some(map) = value.and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    map.iter()
        .map(|(glob, codes)| {
            let codes = toml_string_list(Some(codes));
            (glob.clone(), str_array(&codes))
        })
        .collect()
}

/// Keys in the ruff config that this importer does not represent (any leftover
/// forces the source to be kept).
fn leftover_keys(table: &toml::Table, lint: Option<&toml::Table>) -> Vec<String> {
    let mut leftovers = Vec::new();
    for key in table.keys() {
        if !RECOGNIZED_TOP.contains(&key.as_str()) {
            leftovers.push(key.clone());
        }
    }
    if let Some(lint) = lint {
        for (key, value) in lint {
            if !RECOGNIZED_LINT.contains(&key.as_str()) {
                leftovers.push(format!("lint.{key}"));
            } else if let Some(plugin) = value.as_table().filter(|_| is_plugin(key)) {
                for sub in plugin.keys() {
                    if !PLUGIN_KEYS
                        .iter()
                        .any(|(table, name, _, _)| table == key && name == sub)
                    {
                        leftovers.push(format!("lint.{key}.{sub}"));
                    }
                }
            }
        }
    }
    leftovers.sort();
    leftovers.dedup();
    leftovers
}

fn is_plugin(key: &str) -> bool {
    PLUGIN_KEYS.iter().any(|(table, ..)| *table == key)
}

/// Push `value` as `poly_key` when present; a value of the wrong type is
/// recorded as unrepresented rather than dropped silently.
fn push_carried(
    entries: &mut Vec<(String, Item)>,
    leftovers: &mut Vec<String>,
    label: &str,
    poly_key: &str,
    value: Option<&toml::Value>,
    kind: Kind,
) {
    let Some(value) = value else { return };
    match carry(value, kind) {
        Some(item) => entries.push((poly_key.to_string(), item)),
        None => leftovers.push(format!("{label} (unexpected value type)")),
    }
}

fn carry(value: &toml::Value, kind: Kind) -> Option<Item> {
    match kind {
        Kind::Str => value.as_str().map(str_item),
        Kind::Int => value.as_integer().map(int_item),
        Kind::IntOrStr => value
            .as_integer()
            .map(int_item)
            .or_else(|| value.as_str().map(str_item)),
        Kind::Bool => value.as_bool().map(bool_item),
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

#[cfg(test)]
mod tests {
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
    fn ruff_format_section_is_dropped_and_not_blocking() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            r#"
[tool.ruff]
line-length = 100
[tool.ruff.lint]
select = ["E"]
[tool.ruff.format]
quote-style = "single"
"#,
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert_eq!(result.absorb, Absorb::Full, "format section must not block absorption");
        assert!(
            result.notes.iter().any(|n| n.contains("[tool.ruff.format]")),
            "dropping ruff format must be noted: {:?}",
            result.notes
        );
    }

    const ARMIS_SHAPE: &str = r#"
[tool.ruff]
target-version = "py311"
line-length = 120
format.docstring-code-format = true
format.docstring-code-line-length = "dynamic"
format.quote-style = "double"
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
    }

    #[test]
    fn format_keys_the_formatter_ignores_are_noted_not_silent() {
        let (result, _) = migrate_pyproject(ARMIS_SHAPE);
        let note = result.notes.iter().find(|n| n.contains("[tool.ruff.format]")).unwrap();
        assert!(note.contains("quote-style"), "{note}");
        assert!(
            !note.contains("docstring-code"),
            "carried keys are not reported as dropped: {note}"
        );
    }

    #[test]
    fn plugin_settings_poly_cannot_honor_are_reported_by_name() {
        let (result, _) = migrate_pyproject(
            r#"
[tool.ruff.lint]
select = ["E"]
isort = { known-first-party = ["acme"], force-single-line = true, lines-after-imports = 2 }
pylint = { max-args = 8, max-locals = 20 }
pydocstyle = { convention = "google", ignore-decorators = ["typing.overload"] }
"#,
        );
        let Absorb::Partial(keys) = result.absorb else {
            panic!("unsupported plugin keys must keep the source");
        };
        assert_eq!(
            keys,
            [
                "lint.isort.force-single-line",
                "lint.isort.lines-after-imports",
                "lint.pydocstyle.ignore-decorators",
                "lint.pylint.max-locals",
            ]
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_reported_not_carried() {
        let (result, doc) = migrate_pyproject("[tool.ruff.lint]\nfixable = \"ALL\"\n");
        assert_eq!(
            result.absorb,
            Absorb::Partial(vec!["lint.fixable (unexpected value type)".to_string()])
        );
        assert!(doc.get("lint").is_none());
    }
}
