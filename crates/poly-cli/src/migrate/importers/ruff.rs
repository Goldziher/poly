//! Ruff config importer.
//!
//! Reads `ruff.toml` / `.ruff.toml` (flat or `[lint]`-nested) or, failing that,
//! `pyproject.toml` `[tool.ruff]`. Ports the selection vocabulary and the plugin
//! knobs the poly ruff engine understands into `[lint.python.ruff]`, and
//! ruff's `per-file-ignores` into the top-level `[per-file-ignores]` table.

use std::path::Path;

use toml_edit::Item;

use super::{Absorb, Fragment, ImportResult, bool_item, int_item, str_array, str_item, toml_string_list};

mod keys;

use keys::{FORMAT_KEYS, Kind, LINT_KEYS, PLUGIN_KEYS, RECOGNIZED_LINT, RECOGNIZED_TOP, carry};

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

#[cfg(test)]
mod tests;
