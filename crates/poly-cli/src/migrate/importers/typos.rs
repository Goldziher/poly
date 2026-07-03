//! Typos config importer.
//!
//! Folds `_typos.toml` / `.typos.toml` / `typos.toml` (and `pyproject.toml`
//! `[tool.typos]` / `[tool.codespell]`) into `[lint.typos]` so `poly.toml`
//! becomes the single source of truth. Poly reads native typos files directly,
//! so this is round-trip safe: after folding, `[lint.typos]` suppresses exactly
//! the same words.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::Item;

use super::{Absorb, Fragment, ImportResult, inline_map, str_array, toml_string_list};

/// Standalone typos config filenames in preference order.
const TYPOS_FILES: &[&str] = &["_typos.toml", ".typos.toml", "typos.toml"];

/// `[default]` keys this importer honors.
const RECOGNIZED_DEFAULT: &[&str] = &[
    "extend-words",
    "extend-identifiers",
    "extend-ignore-words",
    "extend-ignore-re",
    "extend-ignore-words-re",
    "extend-ignore-identifiers-re",
];

/// Run the typos importer against `dir`. Reads a standalone typos file when
/// present; otherwise the `pyproject.toml` typos sections.
pub fn import(dir: &Path) -> Option<ImportResult> {
    if let Some(path) = super::first_existing(dir, TYPOS_FILES) {
        let table = super::load_toml(&path)?;
        return Some(build(&table, path, false));
    }
    let pyproject = dir.join("pyproject.toml");
    let table = super::load_toml(&pyproject)?;
    let tool = table.get("tool").and_then(toml::Value::as_table)?;
    if !tool.contains_key("typos") && !tool.contains_key("codespell") {
        return None;
    }
    // Re-root at `[tool]` so `typos` / `codespell` are top-level for `build`.
    let mut rooted = toml::Table::new();
    if let Some(t) = tool.get("typos") {
        rooted.insert("typos".to_string(), t.clone());
    }
    if let Some(c) = tool.get("codespell") {
        rooted.insert("codespell".to_string(), c.clone());
    }
    Some(build(&rooted, pyproject, true))
}

/// Build the `[lint.typos]` fragment. When `pyproject`, the typos config sits
/// under a `typos` sub-table and `codespell.ignore-words-list` is folded in.
fn build(table: &toml::Table, source: PathBuf, pyproject: bool) -> ImportResult {
    let typos_root = if pyproject {
        table.get("typos").and_then(toml::Value::as_table)
    } else {
        Some(table)
    };
    let default = typos_root
        .and_then(|r| r.get("default"))
        .and_then(toml::Value::as_table);
    let files = typos_root.and_then(|r| r.get("files")).and_then(toml::Value::as_table);

    let mut entries: Vec<(String, Item)> = Vec::new();
    if let Some(words) = string_map(default, "extend-words") {
        entries.push(("extend_words".to_string(), inline_map(&words)));
    }
    if let Some(idents) = string_map(default, "extend-identifiers") {
        entries.push(("extend_identifiers".to_string(), inline_map(&idents)));
    }
    push_list(&mut entries, "extend_ignore_words", default, "extend-ignore-words");
    push_list(&mut entries, "extend_ignore_re", default, "extend-ignore-re");
    push_list(
        &mut entries,
        "extend_ignore_words_re",
        default,
        "extend-ignore-words-re",
    );
    push_list(
        &mut entries,
        "extend_ignore_identifiers_re",
        default,
        "extend-ignore-identifiers-re",
    );
    let exclude = toml_string_list(files.and_then(|f| f.get("extend-exclude")));
    if !exclude.is_empty() {
        entries.push(("extend_exclude".to_string(), str_array(&exclude)));
    }

    let mut ignore_words = toml_string_list(default.and_then(|d| d.get("extend-ignore-words")));
    if pyproject {
        ignore_words.extend(codespell_words(table));
        if !ignore_words.is_empty() {
            // Replace any list added above with the codespell-augmented one.
            entries.retain(|(k, _)| k != "extend_ignore_words");
            entries.insert(0, ("extend_ignore_words".to_string(), str_array(&ignore_words)));
        }
    }

    let leftovers = leftover_keys(typos_root, default, files, pyproject);
    let mut fragments = Vec::new();
    if !entries.is_empty() {
        fragments.push(Fragment::new(&["lint", "typos"], entries));
    }
    let absorb = if fragments.is_empty() && leftovers.is_empty() {
        Absorb::None
    } else if leftovers.is_empty() {
        Absorb::Full
    } else {
        Absorb::Partial(leftovers)
    };

    ImportResult {
        tool: "typos",
        sources: vec![source],
        fragments,
        notes: Vec::new(),
        absorb,
    }
}

fn push_list(entries: &mut Vec<(String, Item)>, dst: &str, default: Option<&toml::Table>, key: &str) {
    let list = toml_string_list(default.and_then(|d| d.get(key)));
    if !list.is_empty() {
        entries.push((dst.to_string(), str_array(&list)));
    }
}

fn string_map(default: Option<&toml::Table>, key: &str) -> Option<BTreeMap<String, String>> {
    let table = default?.get(key).and_then(toml::Value::as_table)?;
    let map: BTreeMap<String, String> = table
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
    (!map.is_empty()).then_some(map)
}

/// `pyproject.toml` `[tool.codespell] ignore-words-list` (comma-separated).
fn codespell_words(table: &toml::Table) -> Vec<String> {
    table
        .get("codespell")
        .and_then(toml::Value::as_table)
        .and_then(|c| c.get("ignore-words-list"))
        .and_then(toml::Value::as_str)
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn leftover_keys(
    typos_root: Option<&toml::Table>,
    default: Option<&toml::Table>,
    _files: Option<&toml::Table>,
    pyproject: bool,
) -> Vec<String> {
    let mut leftovers = Vec::new();
    if let Some(root) = typos_root {
        for key in root.keys() {
            // `default` and `files` are dug into individually; `type` overrides
            // and other tables are not representable.
            if key != "default" && key != "files" {
                leftovers.push(key.clone());
            }
        }
    }
    if let Some(default) = default {
        for key in default.keys() {
            if !RECOGNIZED_DEFAULT.contains(&key.as_str()) {
                leftovers.push(format!("default.{key}"));
            }
        }
    }
    // `[files]` — only `extend-exclude` is honored; flag any other key.
    if let Some(files) = _files {
        for key in files.keys() {
            if key != "extend-exclude" {
                leftovers.push(format!("files.{key}"));
            }
        }
    }
    // codespell keys other than the folded ignore-words-list are dropped.
    if pyproject {
        // `codespell` is handled specially; nothing else in the re-rooted table.
    }
    leftovers.sort();
    leftovers.dedup();
    leftovers
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use toml_edit::DocumentMut;

    #[test]
    fn absorbs_typos_with_extend_ignore_re() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("_typos.toml"),
            r#"
[default]
extend-ignore-re = ["(?Rm)^.*# spellchecker:disable-line$"]
extend-ignore-words = ["crate", "ba"]

[default.extend-words]
teh = "teh"

[files]
extend-exclude = ["vendor/**"]
"#,
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert_eq!(result.absorb, Absorb::Full);
        let mut doc = DocumentMut::new();
        super::super::apply(&mut doc, &result.fragments);
        insta::assert_snapshot!("typos_extend_ignore_re", doc.to_string());
    }
}
