//! Builds the `toml_edit` tables for a [`Placed`] layout and numbers them so they render in
//! document order.

use toml_edit::{Decor, Item, Key, Table};

use super::collect::Entry;
use super::place::{EnginePlaced, LangPlaced, Options, Placed};

/// The `[engines]` table: implicit, so only the `[engines.<id>...]` headers render.
pub(super) fn engines_table(placed: &Placed) -> Table {
    let mut engines = Table::new();
    engines.set_implicit(true);
    for (name, engine) in placed {
        if let Some(table) = engine_table(engine) {
            engines.insert(name, Item::Table(table));
        }
    }
    engines
}

fn engine_table(engine: &EnginePlaced) -> Option<Table> {
    let mut table = Table::new();
    put_options(&mut table, &engine.options);
    for (phase, options) in &engine.phases {
        let mut sub = Table::new();
        put_options(&mut sub, options);
        table.insert(phase.v2_key(), Item::Table(sub));
    }
    if !engine.langs.is_empty() {
        let mut langs = Table::new();
        langs.set_implicit(true);
        for (language, placed) in &engine.langs {
            langs.insert(language, Item::Table(lang_table(placed)));
        }
        table.insert("lang", Item::Table(langs));
    }
    put_rules(&mut table, &engine.rules);
    if table.is_empty() {
        return None;
    }
    settle_implicit(&mut table);
    Some(table)
}

fn lang_table(lang: &LangPlaced) -> Table {
    let mut table = Table::new();
    put_options(&mut table, &lang.options);
    for (phase, options) in &lang.phases {
        let mut sub = Table::new();
        put_options(&mut sub, options);
        table.insert(phase.v2_key(), Item::Table(sub));
    }
    put_rules(&mut table, &lang.rules);
    settle_implicit(&mut table);
    table
}

/// A table with nothing but sub-tables renders without its own header.
fn settle_implicit(table: &mut Table) {
    let has_values = table.iter().any(|(_, item)| !item.is_table());
    table.set_implicit(!has_values);
}

fn put_rules(table: &mut Table, rules: &Options) {
    if rules.is_empty() {
        return;
    }
    let mut container = Table::new();
    container.set_implicit(true);
    for (id, entry) in ordered(rules) {
        let mut item = entry.item.clone();
        if let Item::Table(rule) = &mut item {
            rule.set_implicit(false);
            set_header_comments(rule, &entry.comments);
        }
        container.insert(id, item);
    }
    table.insert("rules", Item::Table(container));
}

fn put_options(table: &mut Table, options: &Options) {
    for (name, entry) in ordered(options) {
        let mut item = entry.item.clone();
        let mut key = Key::new(name);
        match &mut item {
            Item::Table(nested) => {
                nested.set_implicit(false);
                set_header_comments(nested, &entry.comments);
            }
            _ if !entry.comments.is_empty() => {
                *key.leaf_decor_mut() = Decor::new(comment_block(&entry.comments), " ");
            }
            _ => {}
        }
        table.insert_formatted(&key, item);
    }
}

fn ordered(options: &Options) -> Vec<(&String, &Entry)> {
    let mut entries: Vec<_> = options.iter().collect();
    entries.sort_by(|a, b| a.1.order.cmp(&b.1.order).then_with(|| a.0.cmp(b.0)));
    entries
}

fn set_header_comments(table: &mut Table, comments: &[String]) {
    if !comments.is_empty() {
        table.decor_mut().set_prefix(comment_block(comments));
    }
}

pub(super) fn comment_block(comments: &[String]) -> String {
    comments.iter().map(|line| format!("{line}\n")).collect()
}

/// How many headers `table` and its descendants will print.
pub(super) fn count_headers(table: &Table) -> isize {
    let own = isize::from(!table.is_implicit());
    own + table
        .iter()
        .filter_map(|(_, item)| item.as_table())
        .map(count_headers)
        .sum::<isize>()
}

/// Number `table` and its descendants from `*next`, parents before children.
pub(super) fn assign_positions(table: &mut Table, next: &mut isize) {
    if !table.is_implicit() {
        table.set_position(Some(*next));
        *next += 1;
    }
    for (_, item) in table.iter_mut() {
        if let Item::Table(child) = item {
            assign_positions(child, next);
        }
    }
}
