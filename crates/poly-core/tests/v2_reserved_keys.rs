//! No engine may declare an option key that collides with the structural names
//! reserved inside a v2 `[engines.<id>]` table (`poly_config::v2::RESERVED_ENGINE_KEYS`).
//!
//! Declared keys are read from the published schema, which poly-core builds from
//! every engine's `option_keys` (the same declaration `unknown-config-key` uses).
//!
//! Two reserved names are allowed to coincide with an existing option because they
//! already mean the same thing: `enabled` (the on/off toggle; it must accept a
//! boolean) and `rules` (v1's rule allow-list; v2 migrates the array form to
//! `select`, see `RESERVED_ENGINE_KEYS`). `lang`, `lint` and `format` must never
//! be option keys.
#![cfg(feature = "all-engines")]

use std::collections::BTreeSet;

use poly_config::v2::RESERVED_ENGINE_KEYS;
use serde_json::Value;

const MAY_OVERLAP: &[&str] = &["enabled", "rules"];

fn accepts_boolean(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("boolean")
        || schema
            .get("oneOf")
            .and_then(Value::as_array)
            .is_some_and(|alts| alts.iter().any(accepts_boolean))
}

#[test]
fn engine_option_keys_do_not_clash_with_reserved_v2_names() {
    let schema = poly_core::config_schema::poly_toml_schema();
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .expect("schema has $defs");

    let mut engine_tables = 0usize;
    let mut clashes: BTreeSet<(String, String)> = BTreeSet::new();
    for (name, def) in defs {
        if !(name.starts_with("lint_") || name.starts_with("fmt_")) {
            continue;
        }
        let Some(properties) = def.get("properties").and_then(Value::as_object) else {
            continue;
        };
        engine_tables += 1;
        for reserved in RESERVED_ENGINE_KEYS {
            if let Some(property) = properties.get(*reserved) {
                if *reserved == "enabled" {
                    assert!(
                        accepts_boolean(property),
                        "`enabled` in {name} must be a boolean toggle"
                    );
                }
                clashes.insert((name.clone(), (*reserved).to_owned()));
            }
        }
    }

    assert!(
        engine_tables > 10,
        "audit saw only {engine_tables} engine tables; the schema shape drifted"
    );
    let forbidden: Vec<_> = clashes
        .iter()
        .filter(|(_, key)| !MAY_OVERLAP.contains(&key.as_str()))
        .collect();
    assert!(
        forbidden.is_empty(),
        "engine option keys collide with reserved v2 names: {forbidden:?}"
    );
}
