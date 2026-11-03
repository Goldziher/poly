//! Raw-table and typed passes the loader applies to the v2 spellings: anchoring
//! `[engines.astgrep] rule_dirs`, and refusing a file that mixes v1 engine tables
//! with `[engines.*]`.

use std::collections::BTreeMap;
use std::path::Path;

use super::EngineTable;

/// The engine whose `rule_dirs` / `builtin_pack` replace v1's top-level `[rules]`.
const ASTGREP: &str = "astgrep";

/// The option holding user ast-grep rule directories (v1: `[rules] dirs`).
pub const RULE_DIRS_KEY: &str = "rule_dirs";

/// The option toggling the embedded ast-grep rule pack (v1: `[rules] builtin`).
pub const BUILTIN_PACK_KEY: &str = "builtin_pack";

/// Make one `rule_dirs` entry absolute against `dir` unless it already is.
fn anchored(entry: &str, dir: &Path) -> String {
    let path = Path::new(entry);
    if path.is_relative() {
        dir.join(path).to_string_lossy().into_owned()
    } else {
        entry.to_string()
    }
}

/// Anchor every relative `rule_dirs` entry of `[engines.astgrep]` (and its `lint`
/// sub-table) in a **raw** config table against `dir`, the directory of the file
/// that declared them. Applied per file during the cascade walk, so a base
/// config's relative paths keep pointing at the base.
pub(crate) fn resolve_rule_dirs_in_raw(table: &mut toml::Table, dir: &Path) {
    let Some(astgrep) = table
        .get_mut("engines")
        .and_then(toml::Value::as_table_mut)
        .and_then(|engines| engines.get_mut(ASTGREP))
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    anchor_array(astgrep, dir);
    if let Some(lint) = astgrep.get_mut("lint").and_then(toml::Value::as_table_mut) {
        anchor_array(lint, dir);
    }
}

fn anchor_array(table: &mut toml::Table, dir: &Path) {
    let Some(entries) = table.get_mut(RULE_DIRS_KEY).and_then(toml::Value::as_array_mut) else {
        return;
    };
    for entry in entries {
        if let Some(text) = entry.as_str() {
            *entry = toml::Value::String(anchored(text, dir));
        }
    }
}

/// [`resolve_rule_dirs_in_raw`] for the typed engine map, for entries the cascade
/// walk did not already anchor (a single-file load, or a config outside a repo).
pub(crate) fn resolve_rule_dirs_typed(engines: &mut BTreeMap<String, EngineTable>, dir: &Path) {
    let Some(astgrep) = engines.get_mut(ASTGREP) else {
        return;
    };
    anchor_array(&mut astgrep.options, dir);
    if let Some(lint) = astgrep.lint.as_mut() {
        anchor_array(&mut lint.options, dir);
    }
}

/// Fail a config that carries both v1 engine tables (`[lint.<lang>.<engine>]`,
/// `[fmt.<lang>.<engine>]`) and `[engines.*]`: which one wins would be a guess.
///
/// `[lint] workspace` is not an engine table and may sit beside `[engines.*]`.
pub(crate) fn reject_mixed(
    lint: &toml::Table,
    fmt: &toml::Table,
    engines: &BTreeMap<String, EngineTable>,
) -> Result<(), String> {
    if engines.is_empty() {
        return Ok(());
    }
    let v1_tables: Vec<String> = lint
        .keys()
        .filter(|key| key.as_str() != "workspace")
        .map(|key| format!("[lint.{key}]"))
        .chain(fmt.keys().map(|key| format!("[fmt.{key}]")))
        .collect();
    if v1_tables.is_empty() {
        return Ok(());
    }
    Err(format!(
        "this poly.toml mixes v1 engine tables ({}) with [engines.*]; run `poly migrate --to-v2` to convert it",
        v1_tables.join(", ")
    ))
}
