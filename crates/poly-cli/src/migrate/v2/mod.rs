//! `poly migrate --to-v2`: rewrite a v1 `poly.toml` into the v2 schema (ADR 0038, epic #48).
//!
//! The rewrite works on a [`toml_edit::DocumentMut`], so comments and key order survive. It is
//! a pure function of the document: [`migrate_document`] reads no files. The loader still reads
//! v1 until the engine-first resolution lands (#63), which is why the step is behind the
//! `--to-v2` flag rather than part of every `poly migrate` run.
//!
//! # What is rewritten
//!
//! * `[lint|fmt.<lang>.<engine>]` and the cross-cutting `[lint|fmt.<engine>]` become
//!   `[engines.<engine>]` with `.lint` / `.format` / `.lang.<id>` sub-tables; the hoisting
//!   algorithm is documented on [`place`].
//! * `[rules]` becomes `[engines.astgrep]`: `dirs` is `rule_dirs`, `builtin` is `builtin_pack`.
//! * `[lint] workspace = true|false` becomes `[workspace] run = "auto"|"never"`. `true` was the
//!   default (the phase runs for repository-wide invocations and is skipped for named paths),
//!   which is exactly `"auto"`; `"always"` has no v1 spelling.
//! * `[workspace] root` becomes the top-level `root`.
//! * `per-file-ignores` becomes `per_file_ignores`.
//! * typos is opt-in in v2, so `[engines.typos] enabled = true` is written unless the file
//!   already switched typos off for the whole engine (`poly.toml` only, not `poly.local.toml`).
//! * `required_version = ">=<this poly>"` is added when absent (`poly.toml` only).
//! * A leading `#:schema` URL naming `poly.schema.json` is pointed at `poly-v2.schema.json`.
//!
//! `poly-config.lock` and `poly-hooks.lock` are not merged here; the migration reports that
//! they must be replaced by `poly lock`.
//!
//! A config without v1 markers ([`poly_config::v2::detect_v1`]) is returned untouched, which
//! makes the step idempotent. After a rewrite the result is deserialized as
//! [`poly_config::v2::PolyConfigV2`] and checked to be marker-free; either failing is an error
//! and the document is left as it was.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use poly_config::v2::{PolyConfigV2, V1Marker, detect_v1};
use toml_edit::{Decor, DocumentMut, Item, Key, Table, value};

mod collect;
mod emit;
mod place;
#[cfg(test)]
mod tests;

use super::MigrationPlan;

/// Engines whose v1 table has no language level; `[lint.X]` is configured directly.
pub(crate) const CROSS_CUTTING_ENGINES: &[&str] = &["typos", "astgrep", "uncomment", "quality"];

const V1_SCHEMA_FILE: &str = "poly.schema.json";
const V2_SCHEMA_FILE: &str = "poly-v2.schema.json";

/// Lock files that `poly lock` replaces.
const V1_LOCK_FILES: &[&str] = &["poly-config.lock", "poly-hooks.lock"];

/// How a document is rewritten.
#[derive(Debug, Clone)]
pub struct V2Options {
    /// Value for `required_version` when the file has none.
    pub required_version: Option<String>,
    /// Write `[engines.typos] enabled = true` to keep v1's default-on typos.
    pub keep_typos_default: bool,
}

impl V2Options {
    /// Options for a project `poly.toml`.
    pub fn for_poly_toml() -> Self {
        V2Options {
            required_version: Some(format!(">={}", env!("CARGO_PKG_VERSION"))),
            keep_typos_default: true,
        }
    }

    /// Options for a `poly.local.toml` overlay, which must not set project-wide policy.
    pub fn for_local_override() -> Self {
        V2Options {
            required_version: None,
            keep_typos_default: false,
        }
    }
}

/// What a rewrite did.
#[derive(Debug, Clone, Default)]
pub struct V2Outcome {
    /// Whether the document changed.
    pub changed: bool,
    /// The v1 constructs that were found.
    pub markers: Vec<V1Marker>,
    /// Human-readable notes: lossy placements and follow-up work.
    pub notes: Vec<String>,
}

/// A rewritten `poly.local.toml`.
pub struct LocalRewrite {
    /// The file the rewrite belongs to.
    pub path: PathBuf,
    /// The rewritten document.
    pub doc: DocumentMut,
    /// What changed.
    pub outcome: V2Outcome,
}

/// Rewrite `doc` from the v1 schema to v2. Unchanged (and `changed == false`) when it has no v1
/// markers.
pub fn migrate_document(doc: &mut DocumentMut, options: &V2Options) -> Result<V2Outcome> {
    let original = doc.to_string();
    let parsed: toml::Table = original.parse().context("the config is not valid TOML")?;
    let markers = detect_v1(&parsed);
    if markers.is_empty() {
        return Ok(V2Outcome::default());
    }
    let (schema, body) = split_schema_line(&original);
    let mut work: DocumentMut = body.parse().context("the config is not valid TOML")?;
    let mut notes = Vec::new();
    let new_root_keys = rewrite(&mut work, options, &mut notes)?;
    let mut text = work.to_string();
    if new_root_keys {
        text = separate_root_keys(&text);
    }
    if let Some(line) = schema {
        text = format!("{}\n{text}", updated_schema_line(&line, &mut notes));
    }
    validate(&text)?;
    *doc = text.parse().context("the migrated config is not valid TOML")?;
    Ok(V2Outcome {
        changed: true,
        markers,
        notes,
    })
}

/// Notes about v1 lock files next to a config that was migrated.
pub fn lock_file_notes(dir: &Path) -> Vec<String> {
    let present: Vec<&str> = V1_LOCK_FILES
        .iter()
        .copied()
        .filter(|name| dir.join(name).is_file())
        .collect();
    if present.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{} must be replaced by `poly lock`, which writes the single poly.lock (not merged by migrate)",
        present.join(" and ")
    )]
}

/// Run the v2 step on a plan: its `poly.toml` document and, when present, `poly.local.toml`.
pub fn apply_to_plan(plan: &mut MigrationPlan) -> Result<()> {
    let mut outcome = migrate_document(&mut plan.doc, &V2Options::for_poly_toml())
        .with_context(|| format!("migrating {} to v2", plan.poly_toml_path.display()))?;
    if outcome.changed {
        outcome.notes.extend(lock_file_notes(&plan.dir));
    }
    plan.v2 = Some(outcome);

    let local_path = plan.dir.join("poly.local.toml");
    if local_path.is_file() {
        let text = std::fs::read_to_string(&local_path).with_context(|| format!("reading {}", local_path.display()))?;
        let mut doc: DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {}", local_path.display()))?;
        let outcome = migrate_document(&mut doc, &V2Options::for_local_override())
            .with_context(|| format!("migrating {} to v2", local_path.display()))?;
        if outcome.changed {
            plan.local_v2 = Some(LocalRewrite {
                path: local_path,
                doc,
                outcome,
            });
        }
    }
    Ok(())
}

/// Returns whether top-level keys were added.
fn rewrite(doc: &mut DocumentMut, options: &V2Options, notes: &mut Vec<String>) -> Result<bool> {
    let root = doc.as_table_mut();
    let collected = collect::collect(root, notes)?;
    let workspace = read_workspace(root, notes)?;
    let anchor = removed_anchor(root);
    for key in ["lint", "fmt", "rules"] {
        root.remove(key);
    }
    drop_workspace_root(root);
    rename_per_file_ignores(root)?;

    let placed = place::place(&collected, options.keep_typos_default, &place::registry_served, notes)?;
    let mut engines = emit::engines_table(&placed);
    let engine_headers = emit::count_headers(&engines);

    let needs_workspace_table = workspace.run.is_some() && !root.get("workspace").is_some_and(Item::is_table);
    let new_headers = engine_headers + isize::from(needs_workspace_table);
    let anchor = anchor.unwrap_or_else(|| max_position(root).map_or(0, |max| max + 1));
    shift_positions(root, anchor, new_headers);
    let mut next = anchor;

    if let Some((mode, comments)) = &workspace.run {
        if needs_workspace_table {
            let mut table = Table::new();
            table.set_position(Some(next));
            next += 1;
            root.insert("workspace", Item::Table(table));
        }
        let Some(table) = root.get_mut("workspace").and_then(Item::as_table_mut) else {
            bail!("[workspace] must be a table");
        };
        put_key(table, "run", value(*mode), comments);
    }
    if engine_headers > 0 {
        emit::assign_positions(&mut engines, &mut next);
        root.insert("engines", Item::Table(engines));
    }

    let mut added = false;
    if let Some(version) = &options.required_version
        && !root.contains_key("required_version")
    {
        put_key(root, "required_version", value(version.as_str()), &[]);
        added = true;
    }
    if let Some((item, comments)) = workspace.root {
        put_key(root, "root", item, &comments);
        added = true;
    }
    Ok(added)
}

fn put_key(table: &mut Table, name: &str, item: Item, comments: &[String]) {
    let mut key = Key::new(name);
    if !comments.is_empty() {
        *key.leaf_decor_mut() = Decor::new(emit::comment_block(comments), " ");
    }
    table.insert_formatted(&key, item);
}

struct WorkspaceKeys {
    run: Option<(&'static str, Vec<String>)>,
    root: Option<(Item, Vec<String>)>,
}

fn read_workspace(root: &Table, notes: &mut Vec<String>) -> Result<WorkspaceKeys> {
    let mut keys = WorkspaceKeys { run: None, root: None };
    if let Some(lint) = root.get("lint").and_then(Item::as_table_like)
        && let Some((key, item)) = lint.get_key_value("workspace")
    {
        let Some(enabled) = item.as_bool() else {
            bail!("[lint] workspace must be a boolean");
        };
        let mode = if enabled { "auto" } else { "never" };
        notes.push(format!(
            "[lint] workspace = {enabled} became [workspace] run = \"{mode}\""
        ));
        let comments = key
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .map(collect::comment_lines)
            .unwrap_or_default();
        keys.run = Some((mode, comments));
    }
    if let Some(workspace) = root.get("workspace").and_then(Item::as_table_like)
        && let Some((key, item)) = workspace.get_key_value("root")
    {
        let mut item = item.clone();
        if let Item::Value(value) = &mut item {
            value.decor_mut().set_prefix(" ");
        } else {
            bail!("[workspace] root must be a boolean");
        }
        let comments = key
            .leaf_decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .map(collect::comment_lines)
            .unwrap_or_default();
        keys.root = Some((item, comments));
    }
    Ok(keys)
}

/// Remove `root` from `[workspace]`, and the table when nothing else is left in it.
fn drop_workspace_root(root: &mut Table) {
    let Some(workspace) = root.get_mut("workspace").and_then(Item::as_table_mut) else {
        return;
    };
    if workspace.remove("root").is_some() && workspace.is_empty() {
        root.remove("workspace");
    }
}

fn rename_per_file_ignores(root: &mut Table) -> Result<()> {
    let Some((old, item)) = root.remove_entry("per-file-ignores") else {
        return Ok(());
    };
    if root.contains_key("per_file_ignores") {
        bail!("both `per-file-ignores` and `per_file_ignores` are present; keep one");
    }
    let mut key = Key::new("per_file_ignores");
    *key.leaf_decor_mut() = old.leaf_decor().clone();
    root.insert_formatted(&key, item);
    Ok(())
}

/// Where the removed v1 tables started, so the engine tables take their place.
fn removed_anchor(root: &Table) -> Option<isize> {
    ["lint", "fmt", "rules"]
        .iter()
        .filter_map(|name| root.get(name))
        .filter_map(item_positions)
        .map(|(min, _)| min)
        .min()
}

fn max_position(root: &Table) -> Option<isize> {
    root.iter()
        .filter_map(|(_, item)| item_positions(item))
        .map(|(_, max)| max)
        .max()
}

/// The smallest and largest header position at or below `item`.
fn item_positions(item: &Item) -> Option<(isize, isize)> {
    let mut found: Vec<isize> = Vec::new();
    collect_positions(item, &mut found);
    Some((*found.iter().min()?, *found.iter().max()?))
}

fn collect_positions(item: &Item, found: &mut Vec<isize>) {
    match item {
        Item::Table(table) => {
            found.extend(table.position());
            for (_, child) in table.iter() {
                collect_positions(child, found);
            }
        }
        Item::ArrayOfTables(array) => {
            for table in array.iter() {
                found.extend(table.position());
                for (_, child) in table.iter() {
                    collect_positions(child, found);
                }
            }
        }
        _ => {}
    }
}

/// Make room: every header at or after `from` moves down by `by`.
fn shift_positions(table: &mut Table, from: isize, by: isize) {
    if let Some(position) = table.position()
        && position >= from
    {
        table.set_position(Some(position + by));
    }
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(child) => shift_positions(child, from, by),
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    shift_positions(child, from, by);
                }
            }
            _ => {}
        }
    }
}

/// The leading `#:schema` directive and the text without it.
fn split_schema_line(text: &str) -> (Option<String>, String) {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            offset += line.len();
            continue;
        }
        if line.trim_start().starts_with("#:schema") {
            let mut body = String::from(&text[..offset]);
            body.push_str(&text[offset + line.len()..]);
            return (Some(line.trim().to_string()), body);
        }
        break;
    }
    (None, text.to_string())
}

fn updated_schema_line(line: &str, notes: &mut Vec<String>) -> String {
    if line.contains(V1_SCHEMA_FILE) {
        notes.push(format!("#:schema now points at {V2_SCHEMA_FILE}"));
        return line.replace(V1_SCHEMA_FILE, V2_SCHEMA_FILE);
    }
    notes.push("#:schema does not name poly.schema.json; update it to the v2 schema by hand".to_string());
    line.to_string()
}

/// Put a blank line between the top-level keys and the first table header (with its comments).
fn separate_root_keys(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let Some(header) = lines.iter().position(|line| line.starts_with('[')) else {
        return text.to_string();
    };
    let mut start = header;
    while start > 0 && lines[start - 1].trim_start().starts_with('#') {
        start -= 1;
    }
    if start == 0 || lines[start - 1].trim().is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 1);
    for (index, line) in lines.iter().enumerate() {
        if index == start {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

fn validate(text: &str) -> Result<()> {
    let table: toml::Table = text.parse().context("the migrated config is not valid TOML")?;
    let left = detect_v1(&table);
    if !left.is_empty() {
        let names: Vec<String> = left.iter().map(ToString::to_string).collect();
        bail!("v1 constructs remain after migration: {}", names.join("; "));
    }
    toml::from_str::<PolyConfigV2>(text).context("the migrated config does not parse as a v2 poly.toml")?;
    Ok(())
}
