//! Reads the v1 engine tables (`[lint|fmt.<lang>.<engine>]`, cross-cutting
//! `[lint|fmt.<engine>]` and the global `[rules]`) into a language-and-phase
//! indexed model that [`super::place`] then rearranges.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use toml_edit::{DocumentMut, Item, Table, TableLike};

use super::CROSS_CUTTING_ENGINES;

/// Names with a structural meaning in a v2 engine table; a v1 option may not use them.
const STRUCTURAL_KEYS: &[&str] = &["lint", "format", "lang"];

/// The two v1 phases, in the order they are emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Phase {
    /// `[lint...]`, v2 `[engines.<id>.lint]`.
    Lint,
    /// `[fmt...]`, v2 `[engines.<id>.format]`.
    Format,
}

impl Phase {
    /// Both phases, lint first.
    pub(super) const ALL: [Phase; 2] = [Phase::Lint, Phase::Format];

    /// The v2 sub-table name.
    pub(super) fn v2_key(self) -> &'static str {
        match self {
            Phase::Lint => "lint",
            Phase::Format => "format",
        }
    }

    /// The v1 top-level table name.
    fn v1_key(self) -> &'static str {
        match self {
            Phase::Lint => "lint",
            Phase::Format => "fmt",
        }
    }
}

/// One option (or rule override) with the comments that sat above it.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    /// `# ...` lines that preceded the key (or the table header it came from).
    pub comments: Vec<String>,
    /// The value, with its trailing comment, ready to insert.
    pub item: Item,
    /// Decor-free canonical form, used to decide whether two entries are equal.
    pub canon: String,
    /// First-appearance rank in the source file; keeps the output in source order.
    pub order: usize,
}

/// `None` is a language-agnostic table (`[lint.typos]`); `Some` names a language.
pub(super) type Scope = Option<String>;

/// The options and rule overrides of one v1 table.
#[derive(Debug, Default, Clone)]
pub(super) struct Cell {
    /// Engine options by key.
    pub options: BTreeMap<String, Entry>,
    /// `rules.<id>` tables by rule id.
    pub rules: BTreeMap<String, Entry>,
}

/// Every v1 table of one engine, by scope and phase.
#[derive(Debug, Default, Clone)]
pub(super) struct EngineCells {
    /// The tables.
    pub cells: BTreeMap<(Scope, Phase), Cell>,
}

/// All engines found in a v1 document.
pub(super) type Collected = BTreeMap<String, EngineCells>;

struct Collector {
    engines: Collected,
    next_order: usize,
}

/// Read the engine tables out of a v1 root table.
pub(super) fn collect(root: &Table, notes: &mut Vec<String>) -> Result<Collected> {
    let mut collector = Collector {
        engines: Collected::new(),
        next_order: 1,
    };
    for phase in Phase::ALL {
        let Some(item) = root.get(phase.v1_key()) else {
            continue;
        };
        let Some(table) = item.as_table_like() else {
            bail!("[{}] must be a table", phase.v1_key());
        };
        collector.read_phase(phase, table, notes)?;
    }
    if let Some(item) = root.get("rules") {
        collector.read_global_rules(item)?;
    }
    Ok(collector.engines)
}

impl Collector {
    fn read_phase(&mut self, phase: Phase, table: &dyn TableLike, notes: &mut Vec<String>) -> Result<()> {
        let v1 = phase.v1_key();
        for (name, child) in table.iter() {
            if phase == Phase::Lint && name == "workspace" {
                continue;
            }
            let Some(child_table) = child.as_table_like() else {
                bail!("[{v1}] `{name}` is not a table; cannot place it in v2");
            };
            if CROSS_CUTTING_ENGINES.contains(&name) {
                let comments = header_comments(child);
                self.read_cell(
                    name,
                    None,
                    phase,
                    child_table,
                    comments,
                    &format!("[{v1}.{name}]"),
                    notes,
                )?;
                continue;
            }
            for (engine, engine_item) in child_table.iter() {
                let Some(engine_table) = engine_item.as_table_like() else {
                    bail!("[{v1}.{name}] `{engine}` is not an engine table; cannot place it in v2");
                };
                let comments = header_comments(engine_item);
                let path = format!("[{v1}.{name}.{engine}]");
                self.read_cell(
                    engine,
                    Some(name.to_string()),
                    phase,
                    engine_table,
                    comments,
                    &path,
                    notes,
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // one call site shape; a parameter struct would only rename them
    fn read_cell(
        &mut self,
        engine: &str,
        scope: Scope,
        phase: Phase,
        table: &dyn TableLike,
        header: Vec<String>,
        path: &str,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let mut pending = header;
        let mut cell = Cell::default();
        for (name, item) in table.iter() {
            if STRUCTURAL_KEYS.contains(&name) {
                bail!("{path}: key `{name}` collides with a v2 structural table; rename it first");
            }
            let mut comments = std::mem::take(&mut pending);
            comments.extend(key_comments(table, name, item));
            if name == "rules"
                && let Some(rules) = item.as_table_like()
            {
                self.read_rules(&mut cell, rules, comments, path)?;
                continue;
            }
            let key = if name == "rules" {
                notes.push(format!(
                    "{path}: the array-form `rules` option was renamed to `select`, the v2 spelling"
                ));
                "select"
            } else {
                name
            };
            if key == "inherit_mode" {
                notes.push(format!(
                    "{path}: `inherit_mode` was carried over as an ordinary key; the v2 merge layer decides what it means"
                ));
            }
            if cell.options.contains_key(key) {
                bail!("{path}: both `rules` and `select` are set; merge them by hand first");
            }
            let entry = self.entry(item, comments);
            cell.options.insert(key.to_string(), entry);
        }
        let slot = self
            .engines
            .entry(engine.to_string())
            .or_default()
            .cells
            .entry((scope, phase))
            .or_default();
        for (key, entry) in cell.options {
            slot.options.insert(key, entry);
        }
        for (id, entry) in cell.rules {
            slot.rules.insert(id, entry);
        }
        Ok(())
    }

    fn read_rules(&mut self, cell: &mut Cell, rules: &dyn TableLike, first: Vec<String>, path: &str) -> Result<()> {
        let mut pending = first;
        for (id, rule) in rules.iter() {
            let Ok(mut table) = rule.clone().into_table() else {
                bail!("{path}: `rules.{id}` is not a table");
            };
            let mut comments = std::mem::take(&mut pending);
            comments.extend(key_comments(rules, id, rule));
            table.decor_mut().clear();
            let entry = self.entry(&Item::Table(table), comments);
            cell.rules.insert(id.to_string(), entry);
        }
        Ok(())
    }

    /// `[rules] dirs / builtin` is a language-agnostic ast-grep table.
    fn read_global_rules(&mut self, item: &Item) -> Result<()> {
        let Some(table) = item.as_table_like() else {
            bail!("[rules] must be a table");
        };
        let mut pending = header_comments(item);
        let mut cell = Cell::default();
        for (name, value) in table.iter() {
            let key = match name {
                "dirs" => "rule_dirs",
                "builtin" => "builtin_pack",
                other => bail!("[rules] has unknown key `{other}`; v2 has no equivalent"),
            };
            let mut comments = std::mem::take(&mut pending);
            comments.extend(key_comments(table, name, value));
            cell.options.insert(key.to_string(), self.entry(value, comments));
        }
        let slot = self
            .engines
            .entry("astgrep".to_string())
            .or_default()
            .cells
            .entry((None, Phase::Lint))
            .or_default();
        for (key, entry) in cell.options {
            if slot.options.contains_key(&key) {
                bail!("[rules] and [lint.astgrep] both set `{key}`; keep one");
            }
            slot.options.insert(key, entry);
        }
        Ok(())
    }

    fn entry(&mut self, item: &Item, comments: Vec<String>) -> Entry {
        let mut item = item.clone();
        match &mut item {
            Item::Value(value) => value.decor_mut().set_prefix(" "),
            Item::Table(table) => table.decor_mut().clear(),
            _ => {}
        }
        let order = self.next_order;
        self.next_order += 1;
        Entry {
            comments,
            canon: canonical(&item),
            item,
            order,
        }
    }
}

/// A rendering of `item` with all formatting stripped, for equality checks.
fn canonical(item: &Item) -> String {
    let mut doc = DocumentMut::new();
    doc.insert("v", item.clone());
    let text = doc.to_string();
    match text.parse::<toml::Table>() {
        Ok(table) => format!("{:?}", table.get("v")),
        Err(_) => text,
    }
}

/// `# ...` lines in `raw`, trimmed; the `#:schema` directive is not a comment to move.
pub(super) fn comment_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('#') && !line.starts_with("#:schema"))
        .map(str::to_string)
        .collect()
}

fn header_comments(item: &Item) -> Vec<String> {
    match item {
        Item::Table(table) => table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .map(comment_lines)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn key_comments(table: &dyn TableLike, name: &str, item: &Item) -> Vec<String> {
    let mut lines = table
        .get_key_value(name)
        .and_then(|(key, _)| key.leaf_decor().prefix())
        .and_then(|prefix| prefix.as_str())
        .map(comment_lines)
        .unwrap_or_default();
    lines.extend(header_comments(item));
    lines
}
