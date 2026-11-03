//! Engine-first (`[engines.<id>]`, ADR 0038) config resolution: one test per precedence step,
//! array union semantics, the astgrep `[rules]` replacement and the v1/v2 mixing error.
//!
//! Precedence, lowest to highest: tool defaults, native config files, `[engines.X]`,
//! `[engines.X.<phase>]`, `[engines.X.lang.<id>]`, `[engines.X.lang.<id>.<phase>]`.
#![cfg(feature = "all-engines")]

use std::collections::BTreeMap;

use poly_config::v2::EngineTable;
use poly_core::config::{Config, Kind};
use poly_core::language::Language;

/// A `Config` whose `[engines]` come from the TOML text (the text names `[engines.*]` tables).
fn config(source: &str) -> Config {
    let document: toml::Table = toml::from_str(source).expect("fixture is valid TOML");
    let engines: BTreeMap<String, EngineTable> = document
        .get("engines")
        .cloned()
        .expect("fixture has [engines]")
        .try_into()
        .expect("fixture is a valid [engines] map");
    Config {
        engines,
        ..Config::default()
    }
}

fn line_length(config: &Config, language: &Language, kind: Kind) -> Option<i64> {
    config
        .engine_config(language, "ruff", kind)
        .options
        .get("line_length")
        .and_then(toml::Value::as_integer)
}

fn strings(options: &toml::Table, key: &str) -> Vec<String> {
    options
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn owned(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn engine_table_applies_to_every_phase_and_language() {
    let config = config("[engines.ruff]\nline_length = 1\n");
    for kind in [Kind::Lint, Kind::Format] {
        assert_eq!(line_length(&config, &Language::Python, kind), Some(1));
        assert_eq!(line_length(&config, &Language::Go, kind), Some(1));
    }
}

#[test]
fn a_missing_engine_table_resolves_to_no_options() {
    let config = config("[engines.other]\nline_length = 1\n");
    assert_eq!(line_length(&config, &Language::Python, Kind::Lint), None);
}

#[test]
fn phase_table_overrides_the_engine_table_for_its_phase_only() {
    let config = config("[engines.ruff]\nline_length = 1\n[engines.ruff.lint]\nline_length = 2\n");
    assert_eq!(line_length(&config, &Language::Python, Kind::Lint), Some(2));
    assert_eq!(line_length(&config, &Language::Python, Kind::Format), Some(1));
}

#[test]
fn language_table_overrides_the_phase_table_for_its_language_only() {
    let config = config(
        "[engines.ruff]\nline_length = 1\n[engines.ruff.lint]\nline_length = 2\n[engines.ruff.lang.python]\nline_length = 3\n",
    );
    assert_eq!(line_length(&config, &Language::Python, Kind::Lint), Some(3));
    assert_eq!(line_length(&config, &Language::Python, Kind::Format), Some(3));
    assert_eq!(line_length(&config, &Language::Go, Kind::Lint), Some(2));
    assert_eq!(line_length(&config, &Language::Go, Kind::Format), Some(1));
}

#[test]
fn language_phase_table_overrides_the_language_table() {
    let config = config(
        "[engines.ruff]\nline_length = 1\n[engines.ruff.lang.python]\nline_length = 3\n[engines.ruff.lang.python.lint]\nline_length = 4\n",
    );
    assert_eq!(line_length(&config, &Language::Python, Kind::Lint), Some(4));
    assert_eq!(line_length(&config, &Language::Python, Kind::Format), Some(3));
    assert_eq!(line_length(&config, &Language::Go, Kind::Lint), Some(1));
}

#[test]
fn engine_table_overrides_a_native_config_file() {
    let mut native = poly_config::TyposNative::default();
    native.extend_words.insert("teh".to_string(), "native".to_string());
    native.extend_words.insert("keep".to_string(), "native".to_string());
    let mut config = config("[engines.typos]\n[engines.typos.extend_words]\nteh = \"engine\"\n");
    config.typos_native = native;
    let options = config.engine_config(&Language::Python, "typos", Kind::Lint).options;
    let words = options
        .get("extend_words")
        .and_then(toml::Value::as_table)
        .expect("extend_words");
    assert_eq!(
        words.get("teh").and_then(toml::Value::as_str),
        Some("engine"),
        "engine table beats the native file"
    );
    assert_eq!(
        words.get("keep").and_then(toml::Value::as_str),
        Some("native"),
        "native file fills what the table left"
    );
}

fn enabled(config: &Config, language: &Language, engine: &str, kind: Kind) -> Option<bool> {
    config
        .engine_config(language, engine, kind)
        .options
        .get("enabled")
        .and_then(toml::Value::as_bool)
}

#[test]
fn enabled_resolves_through_the_same_chain() {
    let config = config(
        "[engines.ruff]\nenabled = false\n[engines.ruff.lint]\nenabled = true\n[engines.ruff.lang.go]\nenabled = true\n[engines.ruff.lang.go.lint]\nenabled = false\n",
    );
    assert_eq!(
        enabled(&config, &Language::Python, "ruff", Kind::Format),
        Some(false),
        "engine level"
    );
    assert_eq!(
        enabled(&config, &Language::Python, "ruff", Kind::Lint),
        Some(true),
        "phase beats engine"
    );
    assert_eq!(
        enabled(&config, &Language::Go, "ruff", Kind::Format),
        Some(true),
        "language beats engine"
    );
    assert_eq!(
        enabled(&config, &Language::Go, "ruff", Kind::Lint),
        Some(false),
        "language phase beats language"
    );
    assert_eq!(
        enabled(&config, &Language::Python, "other", Kind::Lint),
        None,
        "unnamed engine stays unset"
    );
}

#[test]
fn enabled_on_a_cross_cutting_engine_follows_the_chain_too() {
    let config = config("[engines.uncomment]\nenabled = true\n[engines.uncomment.lang.go]\nenabled = false\n");
    assert_eq!(enabled(&config, &Language::Python, "uncomment", Kind::Lint), Some(true));
    assert_eq!(enabled(&config, &Language::Go, "uncomment", Kind::Lint), Some(false));
}

#[test]
fn rule_overrides_merge_from_engine_level_then_language_level() {
    let config = config(
        "[engines.ruff.rules.A]\nlevel = \"warn\"\nextra = 1\n[engines.ruff.rules.B]\nlevel = \"off\"\n[engines.ruff.lang.python.rules.A]\nlevel = \"error\"\n",
    );
    let python = config.engine_config(&Language::Python, "ruff", Kind::Lint).options;
    let rules = python.get("rules").and_then(toml::Value::as_table).expect("rules");
    let a = rules.get("A").and_then(toml::Value::as_table).expect("A");
    assert_eq!(
        a.get("level").and_then(toml::Value::as_str),
        Some("error"),
        "language level wins"
    );
    assert_eq!(
        a.get("extra").and_then(toml::Value::as_integer),
        Some(1),
        "engine-level parameter is kept"
    );
    assert!(rules.contains_key("B"));

    let go = config.engine_config(&Language::Go, "ruff", Kind::Lint).options;
    let a = go["rules"]["A"].as_table().expect("A");
    assert_eq!(a.get("level").and_then(toml::Value::as_str), Some("warn"));
}

#[test]
fn typos_arrays_union_across_every_level_and_maps_override_per_key() {
    let mut native = poly_config::TyposNative::default();
    native.extend_ignore_words.push("native".to_string());
    let mut config = config(
        r#"
[engines.typos]
extend_ignore_words = ["engine"]
[engines.typos.extend_words]
a = "1"
b = "1"
[engines.typos.lint]
extend_ignore_words = ["phase"]
[engines.typos.lang.python]
extend_ignore_words = ["lang"]
[engines.typos.lang.python.extend_words]
b = "2"
[engines.typos.lang.python.lint]
extend_ignore_words = ["langphase"]
"#,
    );
    config.typos_native = native;

    let python = config.engine_config(&Language::Python, "typos", Kind::Lint).options;
    assert_eq!(
        strings(&python, "extend_ignore_words"),
        owned(&["native", "engine", "phase", "lang", "langphase"]),
    );
    let words = python["extend_words"].as_table().expect("extend_words");
    assert_eq!(words.get("a").and_then(toml::Value::as_str), Some("1"));
    assert_eq!(
        words.get("b").and_then(toml::Value::as_str),
        Some("2"),
        "language map overrides per key"
    );

    let go_format = config.engine_config(&Language::Go, "typos", Kind::Format).options;
    assert_eq!(strings(&go_format, "extend_ignore_words"), owned(&["native", "engine"]));
}

#[test]
fn uncomment_preserve_patterns_union_but_booleans_override() {
    let config = config(
        r#"
[engines.uncomment]
enabled = true
remove_todos = false
preserve_patterns = ["SAFETY"]
[engines.uncomment.lang.rust]
remove_todos = true
preserve_patterns = ["HACK"]
[engines.uncomment.lang.rust.lint]
preserve_patterns = ["NOTE"]
"#,
    );
    let rust = config.engine_config(&Language::Rust, "uncomment", Kind::Lint).options;
    assert_eq!(strings(&rust, "preserve_patterns"), owned(&["SAFETY", "HACK", "NOTE"]));
    assert_eq!(rust.get("remove_todos").and_then(toml::Value::as_bool), Some(true));

    let python = config.engine_config(&Language::Python, "uncomment", Kind::Lint).options;
    assert_eq!(strings(&python, "preserve_patterns"), owned(&["SAFETY"]));
    assert_eq!(python.get("remove_todos").and_then(toml::Value::as_bool), Some(false));
}

#[test]
fn quality_values_override_rather_than_union() {
    let config = config(
        "[engines.quality]\nfunction_too_long_lines = 40\n[engines.quality.lang.go]\nfunction_too_long_lines = 200\n",
    );
    let value = |language: &Language| {
        config
            .engine_config(language, "quality", Kind::Lint)
            .options
            .get("function_too_long_lines")
            .and_then(toml::Value::as_integer)
    };
    assert_eq!(value(&Language::Python), Some(40));
    assert_eq!(value(&Language::Go), Some(200));
}

#[test]
fn astgrep_rule_dirs_and_builtin_pack_replace_the_top_level_rules_table() {
    let engines: BTreeMap<String, EngineTable> = toml::from_str::<toml::Table>(
        "[astgrep]\nrule_dirs = [\"/abs/rules\"]\nbuiltin_pack = false\nselect = [\"no-unwrap\"]\n",
    )
    .expect("fixture")
    .try_into()
    .expect("engine map");
    let config = Config::from(poly_config::PolyConfig {
        engines,
        ..Default::default()
    });
    assert_eq!(config.rules_dirs, vec!["/abs/rules".to_string()]);
    assert!(!config.rules_builtin_pack);

    let options = config.engine_config(&Language::Rust, "astgrep", Kind::Lint).options;
    assert_eq!(strings(&options, "rules_dirs"), owned(&["/abs/rules"]));
    assert_eq!(
        options.get("builtin_pack_enabled").and_then(toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(strings(&options, "select"), owned(&["no-unwrap"]));
    assert!(!options.contains_key("rule_dirs") && !options.contains_key("builtin_pack"));
}

#[test]
fn astgrep_without_v2_rule_keys_keeps_the_v1_defaults() {
    let engines: BTreeMap<String, EngineTable> = toml::from_str::<toml::Table>("[typos]\nenabled = true\n")
        .expect("fixture")
        .try_into()
        .expect("engine map");
    let config = Config::from(poly_config::PolyConfig {
        engines,
        ..Default::default()
    });
    assert!(config.rules_builtin_pack);
    assert_eq!(config.rules_dirs, poly_config::RulesConfig::default().dirs);
}

#[test]
fn mixing_v1_engine_tables_with_engines_is_a_config_error() {
    let dir = std::env::temp_dir().join(format!("poly-engine-first-mixed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("poly.toml");
    std::fs::write(
        &path,
        "[lint.python.ruff]\nselect = [\"E\"]\n[engines.typos]\nenabled = true\n",
    )
    .expect("write");
    let result = Config::load_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let message = result.expect_err("a mixed config must be refused").to_string();
    assert!(message.contains("poly migrate --to-v2"), "{message}");
}

#[test]
fn a_v1_config_still_resolves_from_the_lint_and_fmt_tables() {
    let lint: toml::Table = toml::from_str("[python.ruff]\nline_length = 7\n").expect("fixture");
    let config = Config {
        lint,
        ..Config::default()
    };
    assert_eq!(line_length(&config, &Language::Python, Kind::Lint), Some(7));
}
