//! A v1 `poly.toml` and its `poly migrate --to-v2` output must resolve to the same engine
//! configuration: for every language, engine and phase the migrated v2 file resolves the same
//! `EngineConfig` options as the v1 file did, so the loader switch changes no behaviour.
//!
//! The engine set and the language set come from the real registry, so a hoist the importer
//! gets wrong (a value widened to a language the v1 file never named) shows up as a mismatch.
#![cfg(feature = "all-engines")]

use std::collections::BTreeSet;
use std::path::Path;

use poly_cli::migrate::v2::{V2Options, migrate_document};
use poly_core::config::{Config, Kind};
use poly_core::{Language, engine_languages};
use toml_edit::DocumentMut;

/// Engines that have no language level in v1.
const CROSS_CUTTING: &[&str] = &["typos", "astgrep", "uncomment", "quality"];

const PER_LANGUAGE: &str = r#"
[lint.python.ruff]
select = ["E", "F"]
line_length = 120
target_version = "py312"

[lint.python.ruff.rules.ANN201]
level = "error"

[fmt.python.ruff]
line_length = 120
quote_style = "double"

[lint.typescript.oxc]
categories = ["correctness"]
max_warnings = 0

[lint.javascript.oxc]
categories = ["correctness"]
max_warnings = 10

[fmt.typescript.oxc]
semi = true
print_width = 100

[fmt.javascript.oxc]
semi = true
print_width = 80

[lint.markdown.rumdl]
disable = ["MD013"]

[fmt.markdown.rumdl]
disable = ["MD013"]
"#;

const CROSS_CUTTING_TABLES: &str = r#"
[lint.typos]
extend_ignore_words = ["teh"]

[lint.typos.extend_words]
colour = "color"

[lint.python.typos]
extend_ignore_words = ["certifi"]

[lint.rust.typos.extend_words]
colour = "colour"

[lint.quality]
function_too_long_lines = 100
cyclomatic_complexity_max = 15

[lint.go.quality]
function_too_long_lines = 200

[lint.uncomment]
enabled = true
remove_todos = false
preserve_patterns = ["SAFETY"]

[lint.rust.uncomment]
remove_todos = true
preserve_patterns = ["HACK"]

[lint.astgrep]
select = ["no-unwrap"]

[lint.astgrep.rules.no-unwrap]
level = "error"
"#;

const RULES_TABLE: &str = r#"
[rules]
dirs = [".poly/rules", "tools/rules"]
builtin = false

[lint.astgrep]
select = ["no-unwrap"]
"#;

const TOP_LEVEL_KEYS: &str = r#"
[workspace]
root = true

[lint]
workspace = false

[lint.python.ruff]
select = ["E"]

[per-file-ignores]
"tests/**" = ["F401"]
"#;

const DISABLED_ENGINES: &str = r#"
[lint.typos]
enabled = false

[lint.python.typos]
enabled = true

[lint.go.quality]
enabled = false

[fmt.python.ruff]
enabled = false
"#;

/// This repository's own configuration, the largest real v1 file available.
const REPO_POLY_TOML: &str = include_str!("../../../poly.toml");

fn write(dir: &Path, text: &str) -> poly_config::PolyConfig {
    let path = dir.join("poly.toml");
    std::fs::write(&path, text).expect("write poly.toml");
    poly_config::PolyConfig::load_file(&path).expect("config loads")
}

/// `options` with the config directory stripped from `rules_dirs`, so two directories compare.
fn portable(mut options: toml::Table, dir: &Path) -> toml::Table {
    if let Some(entries) = options.get_mut("rules_dirs").and_then(toml::Value::as_array_mut) {
        for entry in entries {
            let relative = entry
                .as_str()
                .map(|text| text.strip_prefix(&*dir.to_string_lossy()).unwrap_or(text).to_string());
            if let Some(relative) = relative {
                *entry = toml::Value::String(relative);
            }
        }
    }
    options
}

/// The engines a v1 file names, plus the cross-cutting four.
fn engine_names(v1: &toml::Table) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = CROSS_CUTTING.iter().map(|name| (*name).to_string()).collect();
    for phase in ["lint", "fmt"] {
        let Some(tables) = v1.get(phase).and_then(toml::Value::as_table) else {
            continue;
        };
        for (key, value) in tables {
            let Some(table) = value.as_table() else { continue };
            if CROSS_CUTTING.contains(&key.as_str()) {
                continue;
            }
            names.extend(table.keys().cloned());
        }
    }
    names
}

fn assert_equivalent(label: &str, v1_text: &str) {
    let v1_dir = tempfile::tempdir().expect("tempdir");
    let v2_dir = tempfile::tempdir().expect("tempdir");

    let mut doc: DocumentMut = v1_text.parse().expect("v1 fixture is TOML");
    let options = V2Options {
        required_version: None,
        keep_typos_default: true,
    };
    migrate_document(&mut doc, &options).expect("fixture migrates");
    let v2_text = doc.to_string();

    let v1_poly = write(v1_dir.path(), v1_text);
    let v2_poly = write(v2_dir.path(), &v2_text);
    assert!(
        v1_poly.engines.is_empty(),
        "{label}: the v1 fixture must not use [engines]"
    );

    assert_eq!(
        v1_poly.per_file_ignores, v2_poly.per_file_ignores,
        "{label}: per_file_ignores"
    );
    assert_eq!(v1_poly.workspace.root, v2_poly.workspace.root, "{label}: root");
    let v1_run_disabled = v1_poly.lint.get("workspace").and_then(toml::Value::as_bool) == Some(false);
    let v2_run_disabled = v2_poly.workspace.run == Some(poly_config::v2::RunMode::Never);
    assert_eq!(v1_run_disabled, v2_run_disabled, "{label}: whole-project phase toggle");

    let v1: Config = v1_poly.clone().into();
    let v2: Config = v2_poly.into();
    assert_eq!(v1.rules_builtin_pack, v2.rules_builtin_pack, "{label}: builtin pack");
    let strip = |config: &Config, dir: &Path| -> Vec<String> {
        config
            .rules_dirs
            .iter()
            .map(|entry| entry.strip_prefix(&*dir.to_string_lossy()).unwrap_or(entry).to_string())
            .collect()
    };
    assert_eq!(
        strip(&v1, v1_dir.path()),
        strip(&v2, v2_dir.path()),
        "{label}: rule dirs"
    );

    let raw: toml::Table = v1_text.parse().expect("v1 fixture is TOML");
    let mut compared = 0usize;
    for engine in engine_names(&raw) {
        for (kind, format) in [(Kind::Lint, false), (Kind::Format, true)] {
            for id in engine_languages(&engine, format) {
                let language = Language::from_catalog_name(&id);
                let mut old = v1.engine_config(&language, &engine, kind);
                let mut new = v2.engine_config(&language, &engine, kind);
                // typos is opt-in in v2, so the importer writes `enabled = true` to keep v1's default.
                if engine == "typos" && !old.options.contains_key("enabled") {
                    new.options.remove("enabled");
                }
                old.options = portable(old.options, v1_dir.path());
                new.options = portable(new.options, v2_dir.path());
                assert_eq!(
                    old.options, new.options,
                    "{label}: {engine} options for {id} ({kind:?}) differ after migration\n--- migrated ---\n{v2_text}"
                );
                assert_eq!(
                    old.indent_width, new.indent_width,
                    "{label}: {engine} indent_width for {id}"
                );
                compared += 1;
            }
        }
    }
    assert!(
        compared > 0,
        "{label}: no (engine, language, phase) combination was compared"
    );
}

#[test]
fn per_language_tables_resolve_identically_after_migration() {
    assert_equivalent("per-language", PER_LANGUAGE);
}

#[test]
fn cross_cutting_tables_resolve_identically_after_migration() {
    assert_equivalent("cross-cutting", CROSS_CUTTING_TABLES);
}

#[test]
fn rules_table_resolves_identically_after_migration() {
    assert_equivalent("rules", RULES_TABLE);
}

#[test]
fn root_workspace_toggle_and_per_file_ignores_survive_migration() {
    assert_equivalent("top-level keys", TOP_LEVEL_KEYS);
}

#[test]
fn enabled_toggles_resolve_identically_after_migration() {
    assert_equivalent("enabled", DISABLED_ENGINES);
}

#[test]
fn this_repos_own_poly_toml_resolves_identically_after_migration() {
    assert_equivalent("poly.toml", REPO_POLY_TOML);
}
