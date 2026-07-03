//! Config importers: each reads a foreign tool's config file(s) and produces
//! `poly.toml` fragments plus an [`Absorb`] verdict describing how completely
//! poly can honor the source. The driver merges fragments into an existing
//! `poly.toml` (comment-preserving via `toml_edit`) and the deletion policy uses
//! the verdict to decide whether the source may be removed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

pub mod markdownlint;
pub mod ruff;
pub mod taplo;
pub mod typos;

/// How completely poly can absorb a source config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Absorb {
    /// Every meaningful setting maps onto a poly key — the source may be deleted.
    Full,
    /// Some settings are not representable in poly; the carried keys are the
    /// leftovers. The source must be kept.
    Partial(Vec<String>),
    /// Nothing was absorbed (no recognized config present).
    None,
}

/// A single `poly.toml` table (identified by its header path) and the key/value
/// entries to insert under it.
#[derive(Debug, Clone)]
pub struct Fragment {
    /// Table header path, e.g. `["lint", "python", "ruff"]` renders as
    /// `[lint.python.ruff]`.
    pub path: Vec<String>,
    /// Ordered key → value entries.
    pub entries: Vec<(String, Item)>,
}

impl Fragment {
    /// Build a fragment at `path` from an ordered list of entries.
    pub fn new(path: &[&str], entries: Vec<(String, Item)>) -> Self {
        Fragment {
            path: path.iter().map(|s| (*s).to_string()).collect(),
            entries,
        }
    }
}

/// The outcome of running one importer against a directory.
#[derive(Debug, Clone)]
pub struct ImportResult {
    /// Tool name for reporting, e.g. `"ruff"`.
    pub tool: &'static str,
    /// Source files that were read (absolute paths).
    pub sources: Vec<PathBuf>,
    /// `poly.toml` fragments to merge.
    pub fragments: Vec<Fragment>,
    /// Human-readable notes (caveats, dropped settings).
    pub notes: Vec<String>,
    /// Completeness verdict driving the deletion policy.
    pub absorb: Absorb,
}

impl ImportResult {
    /// Whether this result carries any table to merge.
    pub fn has_fragments(&self) -> bool {
        self.fragments.iter().any(|f| !f.entries.is_empty())
    }
}

/// Merge every fragment into `doc`, preferring keys already present (idempotent
/// re-runs never churn). Returns a list of conflict notes for keys that were
/// left untouched because the destination already defined them.
pub fn apply(doc: &mut DocumentMut, fragments: &[Fragment]) -> Vec<String> {
    let mut conflicts = Vec::new();
    for fragment in fragments {
        if fragment.entries.is_empty() {
            continue;
        }
        let table = ensure_table(doc.as_table_mut(), &fragment.path);
        for (key, item) in &fragment.entries {
            if table.contains_key(key) {
                conflicts.push(format!(
                    "[{}] {key} already set; keeping existing value",
                    fragment.path.join(".")
                ));
            } else {
                table.insert(key, item.clone());
            }
        }
    }
    conflicts
}

/// Navigate (creating as needed) the table at `path`, marking freshly created
/// intermediate tables implicit so only the leaf renders a `[header]`.
fn ensure_table<'a>(root: &'a mut Table, path: &[String]) -> &'a mut Table {
    let mut current = root;
    let last = path.len().saturating_sub(1);
    for (index, segment) in path.iter().enumerate() {
        let entry = current.entry(segment).or_insert_with(|| {
            let mut table = Table::new();
            if index < last {
                table.set_implicit(true);
            }
            Item::Table(table)
        });
        current = entry
            .as_table_mut()
            .expect("migrate fragment path segment must be a table");
    }
    current
}

// --- value constructors ----------------------------------------------------

/// A TOML array of strings.
pub fn str_array(items: &[String]) -> Item {
    let mut array = Array::new();
    for item in items {
        array.push(item.as_str());
    }
    Item::Value(Value::Array(array))
}

/// A single-line inline table of string → string (keys sorted for determinism).
pub fn inline_map(map: &BTreeMap<String, String>) -> Item {
    let mut table = InlineTable::new();
    for (key, value) in map {
        table.insert(key, Value::from(value.as_str()));
    }
    Item::Value(Value::InlineTable(table))
}

/// A TOML string value.
pub fn str_item(value: &str) -> Item {
    Item::Value(Value::from(value))
}

/// A TOML integer value.
pub fn int_item(value: i64) -> Item {
    Item::Value(Value::from(value))
}

/// A TOML boolean value.
pub fn bool_item(value: bool) -> Item {
    Item::Value(Value::from(value))
}

// --- file loaders ----------------------------------------------------------

/// Read and parse a TOML file, returning `None` when missing or malformed.
pub fn load_toml(path: &Path) -> Option<toml::Table> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// Read and parse a JSON / JSONC file into a [`serde_json::Value`]. Uses `json5`
/// so comments and trailing commas (common in `.jsonc`) parse cleanly.
pub fn load_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    json5::from_str(&text).ok()
}

/// Read and parse a YAML file into a [`serde_json::Value`].
pub fn load_yaml(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_saphyr::from_str(&text).ok()
}

/// Find the first existing file among `names` in `dir`.
pub fn first_existing(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Collect the string elements of a TOML array value, dropping non-strings.
pub fn toml_string_list(value: Option<&toml::Value>) -> Vec<String> {
    value
        .and_then(toml::Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
