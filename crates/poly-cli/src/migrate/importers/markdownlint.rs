//! markdownlint config importer.
//!
//! Reads `.markdownlint.{json,jsonc,yaml,yml}` and maps rule toggles onto the
//! poly rumdl engine's `[lint.markdown.rumdl]` `disable` / `enable` lists,
//! and per-rule parameter tables onto `[lint.markdown.rumdl.rules.<CODE>]`.
//!
//! `"default": false` (turn every rule off, then opt in) has no clean poly
//! equivalent, so it is reported as a Partial verdict and the source is kept.

use std::path::Path;

use serde_json::Value as Json;
use toml_edit::{Array, Item, Value};

use super::{Absorb, Fragment, ImportResult, str_array};

/// Run the markdownlint importer against `dir`.
pub fn import(dir: &Path) -> Option<ImportResult> {
    let candidates = [
        ".markdownlint.json",
        ".markdownlint.jsonc",
        ".markdownlint.yaml",
        ".markdownlint.yml",
    ];
    let source = super::first_existing(dir, &candidates)?;
    let value = if source.extension().is_some_and(|e| e == "yaml" || e == "yml") {
        super::load_yaml(&source)?
    } else {
        super::load_json(&source)?
    };
    let object = value.as_object()?;

    let mut disable: Vec<String> = Vec::new();
    let mut enable: Vec<String> = Vec::new();
    let mut rule_fragments: Vec<Fragment> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut leftovers: Vec<String> = Vec::new();

    for (key, entry) in object {
        if key == "default" {
            if entry.as_bool() == Some(false) {
                leftovers.push("default = false".to_string());
                notes.push(
                    "markdownlint `default = false` (disable-all-then-opt-in) is not \
                     representable in poly; keeping the source."
                        .to_string(),
                );
            }
            continue;
        }
        if !is_md_code(key) {
            // Alias keys (e.g. `line-length`) and extension keys are not mapped.
            leftovers.push(key.clone());
            notes.push(format!("markdownlint key `{key}` is not mapped to rumdl."));
            continue;
        }
        match entry {
            Json::Bool(false) => disable.push(key.clone()),
            Json::Bool(true) => enable.push(key.clone()),
            Json::Object(params) => {
                enable.push(key.clone());
                if let Some(entries) = rule_params(params) {
                    rule_fragments.push(Fragment::new(&["lint", "markdown", "rumdl", "rules", key], entries));
                }
                notes.push(format!(
                    "rumdl parameter names for `{key}` may differ from markdownlint; review."
                ));
            }
            _ => leftovers.push(key.clone()),
        }
    }

    let mut fragments = Vec::new();
    let mut top_entries: Vec<(String, Item)> = Vec::new();
    if !disable.is_empty() {
        disable.sort();
        top_entries.push(("disable".to_string(), str_array(&disable)));
    }
    if !enable.is_empty() {
        enable.sort();
        top_entries.push(("enable".to_string(), str_array(&enable)));
    }
    if !top_entries.is_empty() {
        fragments.push(Fragment::new(&["lint", "markdown", "rumdl"], top_entries));
    }
    // Rule param tables render after the parent header.
    rule_fragments.sort_by(|a, b| a.path.cmp(&b.path));
    fragments.extend(rule_fragments);

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
        tool: "markdownlint",
        sources: vec![source],
        fragments,
        notes,
        absorb,
    })
}

/// Whether `key` is an `MD###` rule code.
fn is_md_code(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("MD") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// Convert a rule's parameter object into TOML entries (scalars and string
/// arrays only; nested objects are skipped).
fn rule_params(params: &serde_json::Map<String, Json>) -> Option<Vec<(String, Item)>> {
    let mut entries: Vec<(String, Item)> = Vec::new();
    for (key, value) in params {
        if let Some(item) = json_to_item(value) {
            entries.push((key.clone(), item));
        }
    }
    (!entries.is_empty()).then_some(entries)
}

/// Convert a JSON scalar or homogeneous array into a `toml_edit` item.
fn json_to_item(value: &Json) -> Option<Item> {
    match value {
        Json::Bool(b) => Some(Item::Value(Value::from(*b))),
        Json::Number(n) if n.is_i64() => Some(Item::Value(Value::from(n.as_i64()?))),
        Json::String(s) => Some(Item::Value(Value::from(s.as_str()))),
        Json::Array(items) => {
            let mut array = Array::new();
            for item in items {
                match item {
                    Json::String(s) => array.push(s.as_str()),
                    Json::Bool(b) => array.push(*b),
                    Json::Number(n) if n.is_i64() => array.push(n.as_i64()?),
                    _ => return None,
                }
            }
            Some(Item::Value(Value::Array(array)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use toml_edit::DocumentMut;

    #[test]
    fn absorbs_markdownlint_json() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".markdownlint.json"),
            r#"{
  "MD013": false,
  "MD033": { "allowed_elements": ["br", "sub"] },
  "MD024": { "siblings_only": true }
}"#,
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert_eq!(result.absorb, Absorb::Full);
        let mut doc = DocumentMut::new();
        super::super::apply(&mut doc, &result.fragments);
        insta::assert_snapshot!("markdownlint_json", doc.to_string());
    }

    #[test]
    fn default_false_is_partial_and_kept() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".markdownlint.json"),
            r#"{ "default": false, "MD013": true }"#,
        )
        .unwrap();
        let result = import(dir.path()).unwrap();
        assert!(
            matches!(result.absorb, Absorb::Partial(_)),
            "default:false must be a Partial verdict"
        );
        assert!(!result.notes.is_empty());
    }
}
