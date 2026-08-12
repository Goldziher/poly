//! Raw-table merging for the config layers, and the one rule that governs how
//! `exclude` lists combine across them.
//!
//! Every config layer is deep-merged as a raw [`toml::Table`] before typed
//! deserialization: scalars and arrays from the higher layer replace the lower
//! one, tables merge key-by-key. That replace-wholesale rule is wrong for
//! **`exclude`** specifically. An exclude list is a policy floor, not a value: a
//! repo that inherits a shared baseline and wants to add one glob of its own had
//! to restate the whole inherited list, freezing a copy of it — after which later
//! changes to the baseline never reached that repo, silently. The repos that most
//! need a shared baseline are exactly the ones that stop tracking it.
//!
//! So there is one rule, applied everywhere excludes are inherited:
//!
//! > **`exclude` lists accumulate; every other key replaces.**
//!
//! It governs [`extends`](crate::extends) bases, `poly.local.toml`, and the
//! `[discovery] exclude` that [`inherit_discovery_excludes`] folds into the
//! `[hooks.builtin.*]` hooks. A layer opts out with a sibling
//! `exclude_mode = "replace"`, which makes that table's `exclude` the whole list
//! and drops what it inherited.
//!
//! The ADR-0018 directory cascade merges with [`merge_tables`] (plain replace)
//! rather than [`merge_layer`]: nested-config excludes are already unioned at
//! walk time by `poly-core`'s `ConfigSet`, each anchored at its own config
//! directory, so accumulating them here too would re-anchor every ancestor glob
//! under every nested config directory and exclude paths nobody named.

use anyhow::bail;

/// Key whose array value accumulates across config layers.
const EXCLUDE_KEY: &str = "exclude";

/// Sibling key that opts a table's [`EXCLUDE_KEY`] out of accumulation.
const EXCLUDE_MODE_KEY: &str = "exclude_mode";

/// Accepted values of [`EXCLUDE_MODE_KEY`].
const EXTEND_MODE: &str = "extend";
const REPLACE_MODE: &str = "replace";

/// The `[hooks.builtin]` hooks that inherit `[discovery] exclude`.
///
/// These are the file-scoped builtins — the ones handed a candidate file set.
/// `commit` (message-only) and `cargo` (whole-workspace) have no `exclude` key.
const EXCLUDE_INHERITING_BUILTINS: [&str; 3] = ["lint", "fmt", "file_safety"];

/// How a layer's `exclude` combines with the one it inherits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExcludeMode {
    /// Default: this layer's globs are added to the inherited ones.
    Extend,
    /// `exclude_mode = "replace"`: this layer's globs are the whole list.
    Replace,
}

/// Deep-merge `override_table` over `base`, replacing scalars and arrays.
///
/// Used for the ADR-0018 directory cascade (see the module docs for why that one
/// layer does not accumulate excludes).
pub(crate) fn merge_tables(base: &mut toml::Table, override_table: toml::Table) {
    merge_into(base, override_table, false);
}

/// Deep-merge `override_table` over `base` as an inherited **config layer**:
/// like [`merge_tables`], except an `exclude` list accumulates on top of the one
/// it inherits instead of replacing it.
///
/// Used for `extends` bases and for `poly.local.toml`.
pub(crate) fn merge_layer(base: &mut toml::Table, override_table: toml::Table) {
    merge_into(base, override_table, true);
}

fn merge_into(base: &mut toml::Table, override_table: toml::Table, accumulate: bool) {
    let extend_excludes = accumulate && exclude_mode(&override_table) == ExcludeMode::Extend;
    for (key, override_value) in override_table {
        match override_value {
            toml::Value::Table(override_child) => match base.get_mut(&key) {
                Some(toml::Value::Table(base_child)) => merge_into(base_child, override_child, accumulate),
                _ => {
                    base.insert(key, toml::Value::Table(override_child));
                }
            },
            other => {
                let accumulated = (extend_excludes && key == EXCLUDE_KEY)
                    .then(|| base.get(&key).and_then(|inherited| union_patterns(inherited, &other)))
                    .flatten();
                base.insert(key, accumulated.unwrap_or(other));
            }
        }
    }
}

/// The declared [`ExcludeMode`] of a table, defaulting to [`ExcludeMode::Extend`].
///
/// An unparseable value cannot reach here: [`validate_directives`] rejects it at
/// parse time, with the offending file named.
fn exclude_mode(table: &toml::Table) -> ExcludeMode {
    match table.get(EXCLUDE_MODE_KEY).and_then(toml::Value::as_str) {
        Some(REPLACE_MODE) => ExcludeMode::Replace,
        _ => ExcludeMode::Extend,
    }
}

/// Concatenate two pattern lists, keeping the inherited globs first and dropping
/// exact duplicates. Returns `None` when either side is not a pattern list (a
/// bare string or an array of strings), leaving the caller to replace instead —
/// a malformed value is the typed schema's error to report, not this layer's.
fn union_patterns(inherited: &toml::Value, own: &toml::Value) -> Option<toml::Value> {
    let mut merged = pattern_list(inherited)?;
    let own = pattern_list(own)?;
    for pattern in own {
        if !merged.contains(&pattern) {
            merged.push(pattern);
        }
    }
    Some(toml::Value::Array(
        merged.into_iter().map(toml::Value::String).collect(),
    ))
}

/// A `Patterns`-shaped value as a list of strings: `"a/**"` or `["a/**", "b/**"]`.
fn pattern_list(value: &toml::Value) -> Option<Vec<String>> {
    match value {
        toml::Value::String(single) => Some(vec![single.clone()]),
        toml::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>(),
        _ => None,
    }
}

/// Reject any malformed `exclude_mode` anywhere in a freshly parsed config.
///
/// Validated per file (rather than after merging) so the error names the file
/// that actually contains the typo.
pub(crate) fn validate_directives(table: &toml::Table) -> anyhow::Result<()> {
    for (key, value) in table {
        if key == EXCLUDE_MODE_KEY {
            match value.as_str() {
                Some(EXTEND_MODE | REPLACE_MODE) => {}
                _ => bail!("`{EXCLUDE_MODE_KEY}` must be {EXTEND_MODE:?} or {REPLACE_MODE:?}, found {value}"),
            }
        }
        for child in child_tables(value) {
            validate_directives(child)?;
        }
    }
    Ok(())
}

/// Remove every `exclude_mode` directive from a fully merged config table.
///
/// The directive drives merging only; it is not part of the typed schema, and
/// several schema structs `deny_unknown_fields`, so it must not survive into
/// deserialization.
pub(crate) fn strip_directives(table: &mut toml::Table) {
    table.remove(EXCLUDE_MODE_KEY);
    for (_, value) in table.iter_mut() {
        for child in child_tables_mut(value) {
            strip_directives(child);
        }
    }
}

/// Fold `[discovery] exclude` into each file-scoped `[hooks.builtin]` hook's own
/// `exclude`, under the same accumulate rule the config layers use.
///
/// A repo's excluded paths are a property of the repo, not of the surface that
/// happens to be walking it: without this, the same list had to be restated in
/// `[discovery]`, `hooks.builtin.lint`, `hooks.builtin.fmt`, and
/// `hooks.builtin.file_safety`. A hook opts out with `exclude_mode = "replace"`
/// in its own table.
pub(crate) fn inherit_discovery_excludes(table: &mut toml::Table) {
    let Some(discovery) = table
        .get("discovery")
        .and_then(toml::Value::as_table)
        .and_then(|discovery| discovery.get(EXCLUDE_KEY))
        .cloned()
    else {
        return;
    };
    if pattern_list(&discovery).is_none_or(|globs| globs.is_empty()) {
        return;
    }
    let Some(builtin) = table
        .get_mut("hooks")
        .and_then(toml::Value::as_table_mut)
        .and_then(|hooks| hooks.get_mut("builtin"))
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    for name in EXCLUDE_INHERITING_BUILTINS {
        let Some(hook) = builtin.get_mut(name) else { continue };
        // A bare `lint = true` carries no table to hold the inherited globs;
        // promote it to its equivalent table form. `lint = false` is disabled,
        // and an absent key is disabled by default — neither needs excludes. ~keep
        if hook.as_bool() == Some(true) {
            let mut promoted = toml::Table::new();
            promoted.insert("enabled".to_string(), toml::Value::Boolean(true));
            *hook = toml::Value::Table(promoted);
        }
        let Some(hook) = hook.as_table_mut() else { continue };
        if exclude_mode(hook) == ExcludeMode::Replace {
            continue;
        }
        let merged = match hook.get(EXCLUDE_KEY) {
            Some(own) => union_patterns(&discovery, own),
            None => Some(discovery.clone()),
        };
        if let Some(merged) = merged {
            hook.insert(EXCLUDE_KEY.to_string(), merged);
        }
    }
}

/// Every table nested directly in `value` (a table itself, or the tables in an
/// array of tables).
fn child_tables(value: &toml::Value) -> Vec<&toml::Table> {
    match value {
        toml::Value::Table(table) => vec![table],
        toml::Value::Array(items) => items.iter().filter_map(toml::Value::as_table).collect(),
        _ => Vec::new(),
    }
}

fn child_tables_mut(value: &mut toml::Value) -> Vec<&mut toml::Table> {
    match value {
        toml::Value::Table(table) => vec![table],
        toml::Value::Array(items) => items.iter_mut().filter_map(toml::Value::as_table_mut).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).expect("parse")
    }

    fn globs(table: &toml::Table, path: &[&str]) -> Vec<String> {
        let mut value = table.get(path[0]).expect("key present");
        for key in &path[1..] {
            value = value.as_table().expect("table").get(*key).expect("key present");
        }
        pattern_list(value).expect("pattern list")
    }

    #[test]
    fn layer_merge_accumulates_exclude_and_replaces_other_arrays() {
        let mut base = table("[discovery]\nexclude = [\"vendor/**\"]\n[lint.python.ruff]\nselect = [\"E\", \"F\"]\n");
        merge_layer(
            &mut base,
            table("[discovery]\nexclude = [\"generated/**\"]\n[lint.python.ruff]\nselect = [\"W\"]\n"),
        );
        assert_eq!(globs(&base, &["discovery", "exclude"]), ["vendor/**", "generated/**"]);
        assert_eq!(globs(&base, &["lint", "python", "ruff", "select"]), ["W"]);
    }

    #[test]
    fn layer_merge_dedupes_and_accepts_the_bare_string_form() {
        let mut base = table("[discovery]\nexclude = \"vendor/**\"\n");
        merge_layer(
            &mut base,
            table("[discovery]\nexclude = [\"vendor/**\", \"dist/**\"]\n"),
        );
        assert_eq!(globs(&base, &["discovery", "exclude"]), ["vendor/**", "dist/**"]);
    }

    #[test]
    fn replace_mode_drops_the_inherited_exclude() {
        let mut base = table("[discovery]\nexclude = [\"vendor/**\"]\n");
        merge_layer(
            &mut base,
            table("[discovery]\nexclude = [\"only/**\"]\nexclude_mode = \"replace\"\n"),
        );
        assert_eq!(globs(&base, &["discovery", "exclude"]), ["only/**"]);
    }

    #[test]
    fn cascade_merge_leaves_exclude_replacing() {
        let mut base = table("[discovery]\nexclude = [\"vendor/**\"]\n");
        merge_tables(&mut base, table("[discovery]\nexclude = [\"nested/**\"]\n"));
        assert_eq!(globs(&base, &["discovery", "exclude"]), ["nested/**"]);
    }

    #[test]
    fn validate_rejects_an_unknown_exclude_mode() {
        let error = validate_directives(&table("[discovery]\nexclude_mode = \"merge\"\n")).unwrap_err();
        assert!(error.to_string().contains("must be"), "{error}");
    }

    #[test]
    fn validate_reaches_tables_inside_arrays() {
        let error =
            validate_directives(&table("[[hooks.pre-commit.jobs]]\nrun = \"x\"\nexclude_mode = true\n")).unwrap_err();
        assert!(error.to_string().contains("must be"), "{error}");
    }

    #[test]
    fn strip_removes_the_directive_everywhere() {
        let mut merged = table(
            "[discovery]\nexclude_mode = \"replace\"\n[hooks.builtin.lint]\nexclude_mode = \"extend\"\n\
             [[hooks.pre-commit.jobs]]\nrun = \"x\"\nexclude_mode = \"replace\"\n",
        );
        strip_directives(&mut merged);
        let rendered = toml::to_string(&merged).expect("render");
        assert!(!rendered.contains(EXCLUDE_MODE_KEY), "{rendered}");
    }
}
