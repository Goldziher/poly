//! Taplo config importer.
//!
//! Ports `.taplo.toml` / `taplo.toml` `[formatting]` into `[fmt.toml.taplo]`.
//! The poly taplo engine consumes exactly these keys.

use std::path::Path;

use toml_edit::Item;

use super::{Absorb, Fragment, ImportResult, bool_item, int_item};

/// Integer `[formatting]` keys that pass through unchanged.
const INT_KEYS: &[&str] = &["column_width", "indent_width", "allowed_blank_lines"];

/// Boolean `[formatting]` keys that pass through unchanged.
const BOOL_KEYS: &[&str] = &[
    "align_entries",
    "align_comments",
    "reorder_keys",
    "indent_tables",
    "indent_entries",
    "array_trailing_comma",
    "array_auto_expand",
    "array_auto_collapse",
    "compact_arrays",
    "compact_inline_tables",
    "inline_table_expand",
];

/// `[formatting]` keys that are recognized but derived/dropped rather than
/// emitted (so they do not force a Partial verdict).
const DERIVED_KEYS: &[&str] = &["indent_string", "trailing_newline", "crlf"];

/// Run the taplo importer against `dir`.
pub fn import(dir: &Path) -> Option<ImportResult> {
    let source = super::first_existing(dir, &[".taplo.toml", "taplo.toml"])?;
    let table = super::load_toml(&source)?;
    let formatting = table.get("formatting").and_then(toml::Value::as_table);

    let mut entries: Vec<(String, Item)> = Vec::new();
    let mut has_indent_width = false;
    if let Some(fmt) = formatting {
        for key in INT_KEYS {
            if let Some(value) = fmt.get(*key).and_then(toml::Value::as_integer) {
                if *key == "indent_width" {
                    has_indent_width = true;
                }
                entries.push(((*key).to_string(), int_item(value)));
            }
        }
        if !has_indent_width
            && let Some(indent) = fmt.get("indent_string").and_then(toml::Value::as_str)
            && !indent.is_empty()
            && indent.chars().all(|c| c == ' ')
        {
            entries.push(("indent_width".to_string(), int_item(indent.len() as i64)));
        }
        for key in BOOL_KEYS {
            if let Some(value) = fmt.get(*key).and_then(toml::Value::as_bool) {
                entries.push(((*key).to_string(), bool_item(value)));
            }
        }
    }

    let leftovers = leftover_keys(&table, formatting);
    let mut fragments = Vec::new();
    if !entries.is_empty() {
        fragments.push(Fragment::new(&["fmt", "toml", "taplo"], entries));
    }
    let absorb = if fragments.is_empty() && leftovers.is_empty() {
        Absorb::None
    } else if leftovers.is_empty() {
        Absorb::Full
    } else {
        Absorb::Partial(leftovers)
    };

    Some(ImportResult {
        tool: "taplo",
        sources: vec![source],
        fragments,
        notes: Vec::new(),
        absorb,
    })
}

/// Keys not represented by `[fmt.toml.taplo]`. `[formatting]` is dug into; any
/// other top-level table (`[schema]`, `[[rule]]`) or unknown formatting key
/// forces the source to be kept.
fn leftover_keys(table: &toml::Table, formatting: Option<&toml::Table>) -> Vec<String> {
    let mut leftovers = Vec::new();
    for key in table.keys() {
        if key != "formatting" {
            leftovers.push(key.clone());
        }
    }
    if let Some(fmt) = formatting {
        for key in fmt.keys() {
            let known = INT_KEYS.contains(&key.as_str())
                || BOOL_KEYS.contains(&key.as_str())
                || DERIVED_KEYS.contains(&key.as_str());
            if !known {
                leftovers.push(format!("formatting.{key}"));
            }
        }
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
    fn absorbs_taplo_formatting() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".taplo.toml"),
            r#"
[formatting]
column_width = 100
indent_string = "    "
align_entries = true
reorder_keys = false
array_auto_collapse = false
trailing_newline = true
"#,
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert_eq!(result.absorb, Absorb::Full);
        let mut doc = DocumentMut::new();
        super::super::apply(&mut doc, &result.fragments);
        insta::assert_snapshot!("taplo_formatting", doc.to_string());
    }
}
