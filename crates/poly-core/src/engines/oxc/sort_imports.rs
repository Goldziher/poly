//! `sort_imports` (the `sortImports` of an oxfmt config) → [`SortImportsOptions`].

use oxc_formatter::{CustomGroupDefinition, GroupEntry, ImportModifier, ImportSelector, SortImportsOptions, SortOrder};

use crate::config::EngineConfig;

/// Build the import-sorting options from `[fmt.<lang>.oxc] sort_imports`, or `None` when the
/// key is absent, disabled (`false`) or invalid (reported once as a warning).
pub(super) fn build_sort_imports(cfg: &EngineConfig) -> Option<SortImportsOptions> {
    match cfg.options.get("sort_imports")? {
        toml::Value::Table(table) => from_table(table),
        toml::Value::Boolean(true) => from_table(&toml::Table::new()),
        _ => None,
    }
}

fn from_table(table: &toml::Table) -> Option<SortImportsOptions> {
    let mut options = SortImportsOptions::default();
    let boolean = |key: &str| table.get(key).and_then(toml::Value::as_bool);
    if let Some(value) = boolean("partition_by_newline") {
        options.partition_by_newline = value;
    }
    if let Some(value) = boolean("partition_by_comment") {
        options.partition_by_comment = value;
    }
    if let Some(value) = boolean("sort_side_effects") {
        options.sort_side_effects = value;
    }
    if let Some(value) = boolean("ignore_case") {
        options.ignore_case = value;
    }
    if let Some(value) = boolean("newlines_between") {
        options.newlines_between = value;
    }
    if table.get("order").and_then(toml::Value::as_str) == Some("desc") {
        options.order = SortOrder::Desc;
    }
    if let Some(patterns) = strings(table.get("internal_pattern")) {
        options.internal_pattern = patterns;
    }
    if let Some(groups) = table.get("custom_groups").and_then(toml::Value::as_array) {
        options.custom_groups = groups.iter().filter_map(custom_group).collect();
    }
    if let Some(groups) = table.get("groups").and_then(toml::Value::as_array) {
        apply_groups(&mut options, groups);
    }
    match options.validate() {
        Ok(()) => Some(options),
        Err(error) => {
            tracing::warn!("invalid sort_imports configuration, imports are left unsorted: {error}");
            None
        }
    }
}

fn strings(value: Option<&toml::Value>) -> Option<Vec<String>> {
    Some(
        value?
            .as_array()?
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
    )
}

fn custom_group(value: &toml::Value) -> Option<CustomGroupDefinition> {
    let table = value.as_table()?;
    Some(CustomGroupDefinition {
        group_name: table.get("group_name")?.as_str()?.to_owned(),
        element_name_pattern: strings(table.get("element_name_pattern")).unwrap_or_default(),
        selector: table
            .get("selector")
            .and_then(toml::Value::as_str)
            .and_then(ImportSelector::parse),
        modifiers: strings(table.get("modifiers"))
            .unwrap_or_default()
            .iter()
            .filter_map(|modifier| ImportModifier::parse(modifier))
            .collect(),
    })
}

/// Groups are strings, arrays of strings (one merged group), or `{ newlines_between = bool }`
/// markers that override the blank-line setting for the boundary that follows.
fn apply_groups(options: &mut SortImportsOptions, items: &[toml::Value]) {
    let mut groups: Vec<Vec<GroupEntry>> = Vec::new();
    let mut overrides: Vec<Option<bool>> = Vec::new();
    let mut pending = None;
    for item in items {
        match item {
            toml::Value::Table(marker) => pending = marker.get("newlines_between").and_then(toml::Value::as_bool),
            other => {
                if !groups.is_empty() {
                    overrides.push(pending.take());
                }
                let names: Vec<&str> = match other {
                    toml::Value::String(name) => vec![name.as_str()],
                    toml::Value::Array(names) => names.iter().filter_map(toml::Value::as_str).collect(),
                    _ => Vec::new(),
                };
                groups.push(names.into_iter().map(GroupEntry::parse).collect());
            }
        }
    }
    options.groups = groups;
    options.newline_boundary_overrides = overrides;
}
