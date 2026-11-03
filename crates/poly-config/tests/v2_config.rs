//! Tests for the v2 `poly.toml` schema types, v1 detection and the committed v2 schema.

use std::collections::BTreeSet;

use poly_config::v2::{
    ComponentInstallPolicy, ComponentsConfig, Enable, OnMissing, PolyConfigV2, RESERVED_ENGINE_KEYS, RunMode, V1Marker,
    V2_TOP_LEVEL_KEYS, config_schema_v2, detect_v1, v1_error,
};

const EPIC_EXAMPLE: &str = r#"
required_version = ">=2.0, <3"
root = true

[components]
enable = ["python", "web"]
disable = ["ruby"]
install = "auto"
on_missing = "fail"

[workspace]
run = "auto"

[engines.ruff]
select = ["E", "F"]
[engines.ruff.format]
[engines.ruff.lang.python]
[engines.ruff.rules.ANN201]
level = "error"

[engines.typos]
enabled = true

[engines.astgrep]
rule_dirs = [".poly/rules"]
builtin_pack = false

[engines.python-types]
backend = "ty"

[tools.shfmt]

[per_file_ignores]
"tests/**" = ["F401"]
"#;

fn table(text: &str) -> toml::Table {
    text.parse().expect("valid toml")
}

#[test]
fn should_parse_the_epic_example() {
    let config: PolyConfigV2 = toml::from_str(EPIC_EXAMPLE).expect("the epic example is valid v2");

    assert_eq!(config.required_version.as_deref(), Some(">=2.0, <3"));
    assert!(config.root);
    assert_eq!(
        config.components.enable,
        Enable::List(vec!["python".into(), "web".into()])
    );
    assert_eq!(config.components.disable, ["ruby"]);
    assert_eq!(config.components.install, ComponentInstallPolicy::Auto);
    assert_eq!(config.components.on_missing, OnMissing::Fail);
    assert_eq!(config.workspace.run, RunMode::Auto);

    let ruff = &config.engines["ruff"];
    assert_eq!(ruff.options["select"].as_array().map(Vec::len), Some(2));
    assert!(ruff.format.is_some());
    assert!(ruff.lint.is_none());
    assert!(ruff.lang.contains_key("python"));
    assert_eq!(ruff.rules["ANN201"].level.as_deref(), Some("error"));
    assert!(!ruff.options.contains_key("rules"), "reserved keys are not options");

    assert_eq!(config.engines["typos"].enabled, Some(true));
    let astgrep = &config.engines["astgrep"];
    assert_eq!(astgrep.options["builtin_pack"].as_bool(), Some(false));
    assert!(astgrep.options.contains_key("rule_dirs"));
    assert_eq!(config.engines["python-types"].options["backend"].as_str(), Some("ty"));

    assert!(config.tools.get("shfmt").is_some());
    assert_eq!(config.per_file_ignores["tests/**"], ["F401"]);
}

#[test]
fn should_default_every_v2_section() {
    let config: PolyConfigV2 = toml::from_str("").expect("an empty file is valid v2");
    assert_eq!(config.components, ComponentsConfig::default());
    assert_eq!(config.workspace.run, RunMode::Auto);
    assert!(!config.root);
    assert!(config.engines.is_empty());
}

#[test]
fn should_reject_an_unknown_top_level_key() {
    assert!(toml::from_str::<PolyConfigV2>("bogus = 1\n").is_err());
}

#[test]
fn should_round_trip_the_components_defaults() {
    let defaults = ComponentsConfig::default();
    assert_eq!(defaults.enable, Enable::Auto);
    assert_eq!(defaults.install, ComponentInstallPolicy::Auto);
    assert_eq!(defaults.on_missing, OnMissing::Fail);
    assert!(defaults.disable.is_empty());

    let text = toml::to_string(&defaults).expect("serializes");
    let back: ComponentsConfig = toml::from_str(&text).expect("re-parses");
    assert_eq!(back, defaults);

    let listed = ComponentsConfig {
        enable: Enable::List(vec!["python".into()]),
        install: ComponentInstallPolicy::Never,
        on_missing: OnMissing::Skip,
        disable: vec!["ruby".into()],
    };
    let back: ComponentsConfig = toml::from_str(&toml::to_string(&listed).expect("serializes")).expect("re-parses");
    assert_eq!(back, listed);
}

#[test]
fn should_reject_a_bad_components_enable_word() {
    assert!(toml::from_str::<ComponentsConfig>("enable = \"everything\"\n").is_err());
}

#[test]
fn should_detect_each_v1_construct() {
    let cases: &[(&str, V1Marker)] = &[
        ("[lint.python.ruff]\nselect = [\"E\"]\n", V1Marker::LintTable),
        ("[fmt.python.ruff]\n", V1Marker::FmtTable),
        ("[lint]\nworkspace = false\n", V1Marker::LintWorkspace),
        ("[rules]\ndirs = []\n", V1Marker::RulesTable),
        ("[workspace]\nroot = true\n", V1Marker::WorkspaceRoot),
        ("[per-file-ignores]\n\"a\" = [\"F401\"]\n", V1Marker::PerFileIgnores),
    ];
    for (text, expected) in cases {
        let found = detect_v1(&table(text));
        assert!(
            found.contains(expected),
            "{text:?} should report {expected:?}, got {found:?}"
        );
    }
}

#[test]
fn should_not_detect_v1_in_a_v2_config() {
    assert!(detect_v1(&table(EPIC_EXAMPLE)).is_empty());
    assert!(detect_v1(&table("")).is_empty());
    assert!(detect_v1(&table("[workspace]\nrun = \"never\"\n")).is_empty());
}

#[test]
fn should_name_the_migration_command_in_the_v1_error() {
    let markers = detect_v1(&table("[lint]\nworkspace = false\n[rules]\n"));
    assert_eq!(
        markers,
        [V1Marker::LintTable, V1Marker::LintWorkspace, V1Marker::RulesTable]
    );
    let message = v1_error(&markers);
    assert!(message.contains("poly migrate --write"));
    assert!(message.contains("[rules]"));
}

#[test]
fn should_keep_reserved_engine_keys_sorted_and_complete() {
    assert_eq!(RESERVED_ENGINE_KEYS, ["enabled", "format", "lang", "lint", "rules"]);
}

#[test]
fn v2_top_level_keys_match_the_typed_schema() {
    // Direction 1: every listed key parses.
    let mut doc = toml::Table::new();
    for key in V2_TOP_LEVEL_KEYS {
        let value = match *key {
            "extends" => toml::Value::Array(Vec::new()),
            "exclude_mode" | "required_version" => toml::Value::String("replace".to_owned()),
            "root" => toml::Value::Boolean(true),
            _ => toml::Value::Table(toml::Table::new()),
        };
        doc.insert((*key).to_owned(), value);
    }
    toml::Value::Table(doc)
        .try_into::<PolyConfigV2>()
        .expect("every V2_TOP_LEVEL_KEYS entry is a recognised field");

    // Direction 2: the schema declares exactly those keys.
    let schema = serde_json::to_value(config_schema_v2()).expect("schema serializes");
    let declared: BTreeSet<String> = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("schema has properties")
        .keys()
        .cloned()
        .collect();
    let listed: BTreeSet<String> = V2_TOP_LEVEL_KEYS.iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(declared, listed);
}

#[test]
fn committed_v2_schema_matches_generated() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/poly-v2.schema.json");
    let generated = serde_json::to_value(config_schema_v2()).expect("schema serializes");
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        let mut text = serde_json::to_string_pretty(&generated).expect("schema renders");
        text.push('\n');
        std::fs::write(path, text).expect("write schema");
        return;
    }
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read schema")).expect("committed schema is JSON");
    assert_eq!(
        generated, committed,
        "schemas/poly-v2.schema.json is stale; regenerate with \
         `UPDATE_SCHEMA=1 cargo test -p poly-config --test v2_config committed_v2_schema`"
    );
}
