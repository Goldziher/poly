//! Run-scoped config overrides (`--set KEY=VALUE`).
//!
//! The CLI parses each `--set` into a one-key TOML table and installs the merged
//! result once, before any config is loaded. [`finalize`](crate::finalize) then
//! deep-merges it over the fully resolved table — above `extends` bases, the
//! `poly.toml` cascade and `poly.local.toml` — so a nested config resolved later
//! in the same run sees the same override.

use std::sync::OnceLock;

use anyhow::{Context, bail};

use crate::merge::merge_tables;

static RUN_OVERRIDES: OnceLock<toml::Table> = OnceLock::new();

/// Parse one `KEY=VALUE` argument into a nested TOML table.
///
/// `KEY` is a dotted TOML key path (`discovery.force_exclude`, quoted segments
/// allowed). `VALUE` is parsed as a TOML value (`true`, `3`, `["a", "b"]`); a
/// value that is not valid TOML is taken as a plain string, so
/// `--set commit.preset=simple` needs no quoting.
pub fn parse_set(arg: &str) -> anyhow::Result<toml::Table> {
    let Some((key, value)) = arg.split_once('=') else {
        bail!("expected KEY=VALUE, got `{arg}`");
    };
    let (key, value) = (key.trim(), value.trim());
    if key.is_empty() {
        bail!("empty key in `--set {arg}`");
    }
    let as_toml = format!("{key} = {value}");
    let parsed = toml::from_str::<toml::Table>(&as_toml).or_else(|_| {
        let quoted = toml::Value::String(value.to_string()).to_string();
        toml::from_str::<toml::Table>(&format!("{key} = {quoted}"))
    });
    parsed.with_context(|| format!("invalid key in `--set {arg}`"))
}

/// Install the run-scoped overrides: every table in `layers` merged in order, so
/// a later `--set` wins over an earlier one. Does nothing for an empty list.
///
/// Process-wide and set-once; a second call is ignored, because configs are
/// resolved from several places in one run and must all see one answer.
pub fn set_run_overrides(layers: Vec<toml::Table>) {
    let mut merged = toml::Table::new();
    for layer in layers {
        merge_tables(&mut merged, layer);
    }
    if !merged.is_empty() {
        let _ = RUN_OVERRIDES.set(merged);
    }
}

/// The installed run-scoped overrides, if any.
#[must_use]
pub fn run_overrides() -> Option<&'static toml::Table> {
    RUN_OVERRIDES.get()
}

/// Merge the installed overrides over `table`.
pub(crate) fn apply_run_overrides(table: &mut toml::Table) {
    if let Some(overrides) = run_overrides() {
        merge_tables(table, overrides.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_dotted_key_and_a_typed_value() {
        let table = parse_set("discovery.force_exclude=true").unwrap();
        assert_eq!(table["discovery"]["force_exclude"], toml::Value::Boolean(true));
    }

    #[test]
    fn parses_arrays_and_integers() {
        let table = parse_set("discovery.exclude=[\"a/**\", \"b\"]").unwrap();
        assert_eq!(table["discovery"]["exclude"].as_array().unwrap().len(), 2);
        assert_eq!(
            parse_set("defaults.line_length=100").unwrap()["defaults"]["line_length"].as_integer(),
            Some(100)
        );
    }

    #[test]
    fn falls_back_to_a_string_value() {
        let table = parse_set("commit.preset=simple").unwrap();
        assert_eq!(table["commit"]["preset"].as_str(), Some("simple"));
    }

    #[test]
    fn rejects_missing_separator_and_empty_key() {
        assert!(parse_set("discovery.generated").is_err());
        assert!(parse_set("=true").is_err());
    }

    #[test]
    fn later_layers_win() {
        let mut base = toml::Table::new();
        merge_tables(&mut base, parse_set("a.b=1").unwrap());
        merge_tables(&mut base, parse_set("a.b=2").unwrap());
        merge_tables(&mut base, parse_set("a.c=3").unwrap());
        assert_eq!(base["a"]["b"].as_integer(), Some(2));
        assert_eq!(base["a"]["c"].as_integer(), Some(3));
    }
}
