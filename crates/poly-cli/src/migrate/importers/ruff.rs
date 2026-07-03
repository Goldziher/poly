//! Ruff config importer.
//!
//! Reads `ruff.toml` / `.ruff.toml` (flat or `[lint]`-nested) or, failing that,
//! `pyproject.toml` `[tool.ruff]`. Ports the selection vocabulary and the plugin
//! knobs the polylint ruff engine understands into `[lint.python.ruff]`, and
//! ruff's `per-file-ignores` into the top-level `[per-file-ignores]` table.

use std::path::Path;

use toml_edit::Item;

use super::{Absorb, Fragment, ImportResult, int_item, str_array, str_item, toml_string_list};

/// Top-level `[tool.ruff]` / `ruff.toml` keys this importer fully honors.
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
];

/// `[lint]`-subtable keys this importer fully honors.
const RECOGNIZED_LINT: &[&str] = &[
    "select",
    "extend-select",
    "ignore",
    "per-file-ignores",
    "pydocstyle",
    "mccabe",
    "pylint",
    "isort",
];

/// Run the ruff importer against `dir`.
pub fn import(dir: &Path) -> Option<ImportResult> {
    let (table, source) = load(dir)?;
    let lint = table.get("lint").and_then(toml::Value::as_table);
    // A key can live either at the top (flat `ruff.toml`) or under `[lint]`.
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
    if let Some(convention) = nested_str(lint, "pydocstyle", "convention") {
        ruff_entries.push(("pydocstyle_convention".to_string(), str_item(convention)));
    }
    if let Some(value) = nested_int(lint, "mccabe", "max-complexity") {
        ruff_entries.push(("mccabe_max_complexity".to_string(), int_item(value)));
    }
    for (src_key, dst_key) in [
        ("max-args", "pylint_max_args"),
        ("max-branches", "pylint_max_branches"),
        ("max-returns", "pylint_max_returns"),
    ] {
        if let Some(value) = nested_int(lint, "pylint", src_key) {
            ruff_entries.push((dst_key.to_string(), int_item(value)));
        }
    }
    let known_first_party = nested_str_list(lint, "isort", "known-first-party");
    if !known_first_party.is_empty() {
        ruff_entries.push(("known_first_party".to_string(), str_array(&known_first_party)));
    }
    if let Some(value) = table
        .get("line-length")
        .or_else(|| table.get("line_length"))
        .and_then(toml::Value::as_integer)
    {
        ruff_entries.push(("line_length".to_string(), int_item(value)));
    }

    // select ∪ extend-select — poly's ruff engine treats `select` as the set and
    // `extend_select` as additions, but flattening into `select` is equivalent
    // and keeps the emitted table minimal.
    let mut select = toml_string_list(pick("select"));
    select.extend(toml_string_list(pick("extend-select")));
    if !select.is_empty() {
        ruff_entries.push(("select".to_string(), str_array(&select)));
    }
    let ignore = toml_string_list(pick("ignore"));
    if !ignore.is_empty() {
        ruff_entries.push(("ignore".to_string(), str_array(&ignore)));
    }

    // per-file-ignores → top-level [per-file-ignores] (poly matches codes across
    // every backend, so it lives outside the ruff table).
    let per_file = collect_per_file(pick("per-file-ignores"));

    let mut fragments = Vec::new();
    if !ruff_entries.is_empty() {
        fragments.push(Fragment::new(&["lint", "python", "ruff"], ruff_entries));
    }
    if !per_file.is_empty() {
        fragments.push(Fragment::new(&["per-file-ignores"], per_file));
    }

    let leftovers = leftover_keys(&table, lint);
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
        notes: Vec::new(),
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
        for key in lint.keys() {
            if !RECOGNIZED_LINT.contains(&key.as_str()) {
                leftovers.push(format!("lint.{key}"));
            }
        }
    }
    leftovers.sort();
    leftovers.dedup();
    leftovers
}

fn nested_str<'a>(lint: Option<&'a toml::Table>, table: &str, key: &str) -> Option<&'a str> {
    lint?
        .get(table)
        .and_then(toml::Value::as_table)?
        .get(key)
        .and_then(toml::Value::as_str)
}

fn nested_int(lint: Option<&toml::Table>, table: &str, key: &str) -> Option<i64> {
    lint?
        .get(table)
        .and_then(toml::Value::as_table)?
        .get(key)
        .and_then(toml::Value::as_integer)
}

fn nested_str_list(lint: Option<&toml::Table>, table: &str, key: &str) -> Vec<String> {
    let value = lint
        .and_then(|l| l.get(table))
        .and_then(toml::Value::as_table)
        .and_then(|t| t.get(key));
    toml_string_list(value)
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
            "select = [\"E\", \"F\"]\nextend-exclude = [\"build\"]\n",
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert_eq!(result.absorb, Absorb::Partial(vec!["extend-exclude".to_string()]));
    }
}
