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
/// (`[tool.ruff.format]`) is carried for the keys in [`FORMAT_KEYS`]; any other
/// key (`preview`, `exclude`, ...) is recorded as a note.
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
    "unsafe-fixes",
    "indent-width",
    "exclude",
    "extend-exclude",
    "extend_exclude",
    "format",
    "extend",
];

/// `[lint]`-subtable keys this importer honors, at least in part. Plugin tables
/// (`pylint`, `isort`, …) are only honored for the keys in [`PLUGIN_KEYS`]; any
/// other key inside them is reported as unrepresented.
const RECOGNIZED_LINT: &[&str] = &[
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
];

/// How a plugin key's value is carried across.
#[derive(Clone, Copy)]
enum Kind {
    Str,
    Int,
    IntOrStr,
    Bool,
    List,
    /// A table whose values are strings or lists of strings.
    Table,
}

/// `(ruff plugin table, ruff key, poly key, kind)` for every plugin setting the
/// poly ruff engine reads (`engines/ruff.rs`, `engines/ruff/plugins.rs`).
const PLUGIN_KEYS: &[(&str, &str, &str, Kind)] = &[
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
];

/// Top-level `[lint]` settings carried as-is: `(ruff key, poly key, kind)`.
const LINT_KEYS: &[(&str, &str, Kind)] = &[
    ("fixable", "fixable", Kind::List),
    ("unfixable", "unfixable", Kind::List),
    ("external", "external", Kind::List),
    ("dummy-variable-rgx", "dummy_variable_rgx", Kind::Str),
    ("unsafe-fixes", "unsafe_fixes", Kind::Bool),
    ("explicit-preview-rules", "explicit_preview_rules", Kind::Bool),
];

/// `[format]` keys the poly ruff formatter reads: `(ruff key, poly key, kind)`.
const FORMAT_KEYS: &[(&str, &str, Kind)] = &[
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
];

/// Run the ruff importer against `dir`.
pub fn import(dir: &Path) -> Option<ImportResult> {
    let (table, source) = load(dir)?;
    let mut notes = Vec::new();
    let mut leftovers = Vec::new();
    let table = resolve_extend(table, &source, 0, &mut notes, &mut leftovers);
    let lint = table.get("lint").and_then(toml::Value::as_table);
    let pick = |key: &str| lint.and_then(|l| l.get(key)).or_else(|| table.get(key));

    let mut ruff_entries: Vec<(String, Item)> = Vec::new();

    if let Some(version) = table
        .get("target-version")
        .or_else(|| table.get("target_version"))
        .and_then(toml::Value::as_str)
    {
        ruff_entries.push(("target_version".to_string(), str_item(version)));
    } else if let Some(version) = infer_target_version(dir) {
        notes.push(format!(
            "no target-version set; ruff infers {version} from `requires-python`, so it is written out"
        ));
        ruff_entries.push(("target_version".to_string(), str_item(&version)));
    }
    let src = toml_string_list(table.get("src"));
    if !src.is_empty() {
        ruff_entries.push(("src".to_string(), str_array(&src)));
    }
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
    let mut ignore = toml_string_list(pick("ignore"));
    ignore.extend(toml_string_list(pick("extend-ignore")));
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

    push_carried(
        &mut ruff_entries,
        &mut leftovers,
        "lint.preview",
        "preview",
        pick("preview"),
        Kind::Bool,
    );

    let per_file = collect_per_file(pick("per-file-ignores"), pick("extend-per-file-ignores"));

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
    if let Some(format) = table.get("format").and_then(toml::Value::as_table) {
        for (key, value) in format {
            match FORMAT_KEYS.iter().find(|(ruff_key, _, _)| ruff_key == key) {
                Some((_, poly_key, kind)) => push_carried(
                    &mut fmt_entries,
                    &mut leftovers,
                    &format!("format.{key}"),
                    poly_key,
                    Some(value),
                    *kind,
                ),
                None => leftovers.push(format!("format.{key}")),
            }
        }
    }

    pin_ruff_defaults(&mut ruff_entries, &mut fmt_entries);

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

/// The deepest `extend` chain followed; ruff itself rejects cycles.
const MAX_EXTEND_DEPTH: usize = 8;

/// Settings ruff accepts at the top level of a config that live in `[lint]`.
const LINT_AT_TOP_LEVEL: &[&str] = &[
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
];

/// Resolve `extend = "<path>"` the way ruff does: the extended file is read
/// first and this file's settings win, except that `extend-*` lists add to the
/// extended ones. The result is one table with no `extend` key, so a nested
/// directory can be written out as a self-contained `poly.toml`.
fn resolve_extend(
    mut table: toml::Table,
    source: &Path,
    depth: usize,
    notes: &mut Vec<String>,
    leftovers: &mut Vec<String>,
) -> toml::Table {
    let Some(target) = table.remove("extend") else {
        return table;
    };
    let Some(relative) = target.as_str() else {
        leftovers.push("extend (unexpected value type)".to_string());
        return table;
    };
    let base_path = source.parent().unwrap_or(Path::new(".")).join(relative);
    let Some(base) = (depth < MAX_EXTEND_DEPTH).then(|| load_file(&base_path)).flatten() else {
        leftovers.push(format!("extend = {relative} (not found or too deep)"));
        return table;
    };
    let base = resolve_extend(base, &base_path, depth + 1, notes, leftovers);
    notes.push(format!("resolved `extend = {relative}` into this directory's settings"));
    combine(normalize_lint(base), normalize_lint(table))
}

/// Move the deprecated top-level lint settings into `[lint]`, where `extend`
/// merging looks for them.
fn normalize_lint(mut table: toml::Table) -> toml::Table {
    let moved: Vec<(String, toml::Value)> = LINT_AT_TOP_LEVEL
        .iter()
        .filter_map(|key| table.remove(*key).map(|value| ((*key).to_string(), value)))
        .collect();
    if moved.is_empty() {
        return table;
    }
    let lint = table
        .entry("lint".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(lint) = lint.as_table_mut() {
        for (key, value) in moved {
            lint.entry(key).or_insert(value);
        }
    }
    table
}

/// Layer `own` over `base`: tables merge, `extend-*` lists concatenate, anything
/// else in `own` replaces.
fn combine(mut base: toml::Table, own: toml::Table) -> toml::Table {
    for (key, value) in own {
        let merged = match (base.remove(&key), value) {
            (Some(toml::Value::Table(below)), toml::Value::Table(above)) => toml::Value::Table(combine(below, above)),
            (Some(toml::Value::Array(mut below)), toml::Value::Array(above)) if key.starts_with("extend-") => {
                below.extend(above);
                toml::Value::Array(below)
            }
            (_, above) => above,
        };
        base.insert(key, merged);
    }
    base
}

/// Parse a ruff config file: `ruff.toml` as is, `pyproject.toml` through its
/// `[tool.ruff]` table.
fn load_file(path: &Path) -> Option<toml::Table> {
    let table = super::load_toml(path)?;
    if path.file_name().is_some_and(|name| name == "pyproject.toml") {
        table.get("tool")?.as_table()?.get("ruff")?.as_table().cloned()
    } else {
        Some(table)
    }
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
fn collect_per_file(value: Option<&toml::Value>, extra: Option<&toml::Value>) -> Vec<(String, Item)> {
    let mut merged: Vec<(String, Vec<String>)> = Vec::new();
    for map in [value, extra].into_iter().filter_map(|v| v?.as_table()) {
        for (glob, codes) in map {
            let codes = toml_string_list(Some(codes));
            match merged.iter_mut().find(|(existing, _)| existing == glob) {
                Some((_, existing)) => {
                    for code in codes {
                        if !existing.contains(&code) {
                            existing.push(code);
                        }
                    }
                }
                None => merged.push((glob.clone(), codes)),
            }
        }
    }
    merged
        .into_iter()
        .map(|(glob, codes)| (anchor_free(glob), str_array(&codes)))
        .collect()
}

/// The target version ruff infers from `project.requires-python` in the
/// directory's `pyproject.toml` when no `target-version` is set: the lowest
/// minor version the specifier admits, spelled `py310`.
fn infer_target_version(dir: &Path) -> Option<String> {
    let pyproject = super::load_toml(&dir.join("pyproject.toml"))?;
    let requires = pyproject.get("project")?.as_table()?.get("requires-python")?.as_str()?;
    let mut lowest: Option<(u32, u32)> = None;
    for clause in requires.split(',').map(str::trim) {
        let Some((operator, version)) = ["~=", ">=", "==", ">"]
            .iter()
            .find_map(|op| clause.strip_prefix(op).map(|rest| (*op, rest.trim())))
        else {
            continue;
        };
        let mut parts = version.trim_end_matches(".*").split('.');
        let (major, minor): (u32, u32) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        let bound = if operator == ">" {
            (major, minor + 1)
        } else {
            (major, minor)
        };
        lowest = lowest.max(Some(bound));
    }
    let (major, minor) = lowest?;
    Some(format!("py{major}{minor}"))
}

/// Write down the ruff defaults that poly's own defaults would otherwise replace.
///
/// Poly's opinionated defaults differ from ruff's: line length 120 against 88,
/// docstring code formatting on against off (and a fixed against a dynamic
/// width), and LF line endings against following each file. A repo that ruff
/// has been formatting and linting must keep getting ruff's answer, so every
/// setting its config left unset is pinned to ruff's value.
fn pin_ruff_defaults(lint: &mut Vec<(String, Item)>, fmt: &mut Vec<(String, Item)>) {
    let is_set = |entries: &[(String, Item)], key: &str| entries.iter().any(|(name, _)| name == key);
    if !is_set(lint, "line_length") {
        lint.push(("line_length".to_string(), int_item(RUFF_LINE_LENGTH)));
    }
    if !is_set(fmt, "line_length") {
        fmt.push(("line_length".to_string(), int_item(RUFF_LINE_LENGTH)));
    }
    if !is_set(fmt, "docstring_code_format") {
        fmt.push(("docstring_code_format".to_string(), bool_item(false)));
    } else if !is_set(fmt, "docstring_code_line_length") {
        let enabled = fmt
            .iter()
            .any(|(name, item)| name == "docstring_code_format" && item.as_bool() == Some(true));
        if enabled {
            fmt.push(("docstring_code_line_length".to_string(), str_item("dynamic")));
        }
    }
    if !is_set(fmt, "line_ending") {
        fmt.push(("line_ending".to_string(), str_item("auto")));
    }
}

/// Ruff's default `line-length`.
const RUFF_LINE_LENGTH: i64 = 88;

/// Ruff matches a per-file-ignores pattern against the file's basename as well as
/// its path, so a pattern with no `/` applies at every depth; poly matches the
/// path only, relative to the config. `**/` restores the depth.
fn anchor_free(glob: String) -> String {
    if glob.contains('/') { glob } else { format!("**/{glob}") }
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

fn carry(value: &toml::Value, kind: Kind) -> Option<Item> {
    match kind {
        Kind::Str => value.as_str().map(str_item),
        Kind::Int => value.as_integer().map(int_item),
        Kind::IntOrStr => value
            .as_integer()
            .map(int_item)
            .or_else(|| value.as_str().map(str_item)),
        Kind::Bool => value.as_bool().map(bool_item),
        Kind::Table => table_item(value.as_table()?),
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
    fn format_keys_poly_cannot_honor_keep_the_source() {
        let (result, _) = migrate_pyproject("[tool.ruff.format]\npreview = true\nexclude = [\"gen/**\"]\n");
        assert_eq!(
            result.absorb,
            Absorb::Partial(vec!["format.exclude".to_string(), "format.preview".to_string()])
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
    fn every_isort_key_poly_honors_is_carried_and_required_imports_is_reported() {
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
        assert_eq!(
            result.absorb,
            Absorb::Partial(vec!["lint.isort.required-imports".to_string()])
        );
        let ruff = &doc["lint"]["python"]["ruff"];
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
isort = { known-first-party = ["acme"], force-single-line = true, lines-after-imports = 2, required-imports = ["from __future__ import annotations"] }
pylint = { max-args = 8, max-locals = 20, allow-magic-value-types = ["str"] }
pydocstyle = { convention = "google", ignore-decorators = ["typing.overload"] }
"#,
        );
        let Absorb::Partial(keys) = result.absorb else {
            panic!("unsupported plugin keys must keep the source");
        };
        assert_eq!(
            keys,
            [
                "lint.isort.required-imports",
                "lint.pydocstyle.ignore-decorators",
                "lint.pylint.allow-magic-value-types",
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
        assert!(doc["lint"]["python"]["ruff"].get("fixable").is_none());
    }
}
