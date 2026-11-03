use poly_component_protocol::WireOptionTable;

use super::*;

fn engine(name: &str, languages: &[&str], declared: &[&str]) -> serde_json::Value {
    let keys = |table: &str| {
        serde_json::json!({
            "table": table, "checked": true,
            "declared": declared.iter().map(|d| serde_json::json!({"name": d, "types": 1})).collect::<Vec<_>>(),
            "rule_selection": false, "has_derived": false, "note": null
        })
    };
    serde_json::json!({
        "name": name, "languages": languages,
        "capabilities": {"lint": true, "format": true, "fix": false, "project": false},
        "version": "1", "option_keys": [keys("Lint"), keys("Format")],
        "hooks": 0, "takes_over_tier_one": false, "claimed_paths": [], "host_hook": "none",
        "supplements_tier_one": false, "self_manages_enabled": false,
        "supersedes_generic_formatter": false, "batch_shard_floor": 2
    })
}

fn fixture() -> ComponentIndex {
    let json = serde_json::json!({
        "schema": 1,
        "components": [
            {"name": "web", "features": ["engine-web"], "engines": [
                engine("oxc", &["javascript", "typescript"], &["line_length"]),
                engine("malva", &["css"], &[])]},
            {"name": "biome", "features": ["engine-biome"], "engines": [
                engine("biome", &["css"], &["a"]), engine("biome", &["graphql"], &["b"])]},
            {"name": "typos", "opt_in": true, "engines": [engine("typos", &[], &[])]},
            {"name": "mcp", "kind": "frontend"}
        ],
        "presets": {"all": ["web", "biome"], "css": ["web", "biome"], "js": ["web"]}
    });
    ComponentIndex::parse(&json.to_string()).unwrap()
}

#[test]
fn placeholder_and_embedded_index_parse() {
    assert_eq!(ComponentIndex::parse("{}").unwrap(), ComponentIndex::default());
    let _ = index();
}

#[test]
fn finds_the_component_for_an_engine() {
    let index = fixture();
    assert_eq!(index.component_for_engine("oxc").unwrap().name, "web");
    assert_eq!(index.component_for_engine("biome").unwrap().name, "biome");
    assert!(index.component_for_engine("nope").is_none());
}

#[test]
fn language_lookup_returns_every_serving_component_in_order() {
    let index = fixture();
    let names = |lang: &str| -> Vec<&str> {
        index
            .components_for_language(lang)
            .iter()
            .map(|c| c.name.as_str())
            .collect()
    };
    assert_eq!(names("css"), ["web", "biome"]);
    assert_eq!(names("graphql"), ["biome"]);
    assert!(names("rust").is_empty());
    let cross: Vec<_> = index
        .cross_cutting_components()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(cross, ["typos"]);
}

#[test]
fn presets_expand_by_name_and_fall_back_to_a_component_name() {
    let index = fixture();
    let names = |p: &str| -> Vec<String> { index.expand_preset(p).unwrap().iter().map(|c| c.name.clone()).collect() };
    assert_eq!(names("all"), ["web", "biome"]);
    assert_eq!(names("js"), ["web"]);
    assert_eq!(names("typos"), ["typos"]);
    assert!(index.expand_preset("unknown").is_none());
}

#[test]
fn engines_of_a_component_and_of_a_frontend() {
    let index = fixture();
    assert_eq!(index.engines("web").len(), 2);
    assert!(index.engines("mcp").is_empty());
    assert!(index.engines("missing").is_empty());
    assert_eq!(index.component("mcp").unwrap().kind, ComponentKind::Frontend);
}

#[test]
fn option_keys_pick_the_description_for_the_language() {
    let index = fixture();
    let declared = |lang: Option<&str>| -> Vec<String> {
        index
            .option_keys("biome", lang, WireOptionTable::Lint)
            .unwrap()
            .declared
            .iter()
            .map(|k| k.name.clone())
            .collect()
    };
    assert_eq!(declared(Some("css")), ["a"]);
    assert_eq!(declared(Some("graphql")), ["b"]);
    assert_eq!(declared(None), ["a"]);
    assert!(
        index
            .option_keys("oxc", None, WireOptionTable::CrossCuttingLint)
            .is_none()
    );
    assert!(
        index
            .option_keys("biome", Some("rust"), WireOptionTable::Lint)
            .is_none()
    );
}
