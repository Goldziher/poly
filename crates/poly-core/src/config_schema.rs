//! The public JSON Schema for `poly.toml`, enriched with the per-engine option
//! tables.
//!
//! [`poly_config::config_schema`] describes every *typed* section — `[defaults]`,
//! `[discovery]`, `[hooks]`, `[commit]`, `[cache]`, `[tools]`, `[rules]`,
//! `[per-file-ignores]`, `extends` — but `[lint]` and `[fmt]` are open
//! `toml::Table`s there, because poly-core is what knows which
//! `[lint.<lang>.<engine>]` keys exist. That answer already exists: every backend
//! declares the keys it reads and the type it reads each as
//! ([`Engine::option_keys`] — the same declaration the `unknown-config-key` check
//! uses), so the schema is generated from one source of truth rather than a
//! second hand-maintained list.
//!
//! Engine tables are emitted with `additionalProperties: true` even though the
//! `properties` carry the known keys. Two reasons: a backend that derives its
//! keys from a serde type (the `serde(flatten)` formatters, malva / markup_fmt /
//! pretty_yaml / pretty_graphql / mago) cannot enumerate them statically, and a
//! language id may be any tree-sitter grammar ([`Language::Other`]). The
//! `properties` are then autocomplete hints, and the runtime `unknown-config-key`
//! check remains the authority on what poly accepts.
//!
//! Each engine's table is hoisted into `$defs` and referenced, rather than
//! inlined under every language that carries it: the same backend has the same
//! keys everywhere it appears, and inlining repeated it a dozen times.

use serde_json::{Map, Value, json};

use crate::config::Kind;
use crate::engine::{Engine, OptionKeys, OptionTable, OptionType};
use crate::language::Language;
use crate::registry::{all_languages, engines_for};

/// The full `poly.toml` JSON Schema, with `[lint]` / `[fmt]` populated from the
/// engine registry. Served by `poly config schema`.
#[must_use]
pub fn poly_toml_schema() -> Value {
    let mut root = serde_json::to_value(poly_config::config_schema()).unwrap_or(Value::Null);
    let mut defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let lint = table_schema(Kind::Lint, &mut defs);
    let fmt = table_schema(Kind::Format, &mut defs);

    if let Some(properties) = root.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert("lint".to_owned(), lint);
        properties.insert("fmt".to_owned(), fmt);
    }
    if let Some(object) = root.as_object_mut() {
        object.insert("$defs".to_owned(), Value::Object(defs));
    }
    root
}

/// The `[lint]` or `[fmt]` table schema: a language id → engine table map, plus
/// the language-agnostic cross-cutting engine tables for lint. Engine tables are
/// registered in `defs` and referenced.
fn table_schema(kind: Kind, defs: &mut Map<String, Value>) -> Value {
    let mut properties = Map::new();

    for language in all_languages() {
        let mut engines = Map::new();
        for engine in engines_for(&language) {
            if !offers(engine.as_ref(), kind) {
                continue;
            }
            let key = def_key(kind, engine.name(), false);
            defs.entry(key.clone()).or_insert_with(|| {
                let keys = engine.option_keys(option_table(kind));
                engine_table(&keys, true)
            });
            engines.insert(engine.name().to_owned(), json!({ "$ref": format!("#/$defs/{key}") }));
        }
        if !engines.is_empty() {
            properties.insert(
                language.id().to_owned(),
                json!({
                    "type": "object",
                    "description": format!("Backends that handle {} files.", language.id()),
                    "properties": engines,
                    "additionalProperties": true
                }),
            );
        }
    }

    // Cross-cutting backends are also configurable language-agnostically
    // (`[lint.typos]`, `[lint.quality]`, …); their universal keys (indent_width,
    // rules) do not apply at that level.
    if matches!(kind, Kind::Lint) {
        for engine in cross_cutting() {
            let key = def_key(kind, engine.name(), true);
            defs.entry(key.clone()).or_insert_with(|| {
                let keys = engine.option_keys(OptionTable::CrossCuttingLint);
                engine_table(&keys, false)
            });
            properties.insert(engine.name().to_owned(), json!({ "$ref": format!("#/$defs/{key}") }));
        }
    }

    json!({
        "type": "object",
        "description": match kind {
            Kind::Lint => "Per-language linter configuration, keyed by language id then engine name.",
            Kind::Format => "Per-language formatter configuration, keyed by language id then engine name.",
        },
        "properties": properties,
        "additionalProperties": true
    })
}

/// A `$defs` key for one engine's table: distinct per kind, and distinct for the
/// language-agnostic cross-cutting form.
fn def_key(kind: Kind, engine: &str, cross_cutting: bool) -> String {
    let kind = match kind {
        Kind::Lint => "lint",
        Kind::Format => "fmt",
    };
    if cross_cutting {
        format!("{kind}_cross_{engine}")
    } else {
        format!("{kind}_{engine}")
    }
}

/// Whether an engine offers a table for `kind`.
fn offers(engine: &dyn Engine, kind: Kind) -> bool {
    let caps = engine.capabilities();
    match kind {
        Kind::Lint => caps.lint,
        Kind::Format => caps.format,
    }
}

/// The language-agnostic cross-cutting backends, taken from any language's
/// registry list (they are appended to every one).
fn cross_cutting() -> Vec<Box<dyn Engine>> {
    engines_for(&Language::Python)
        .into_iter()
        .filter(|engine| engine.languages().is_empty())
        .collect()
}

/// The option table a kind maps to.
fn option_table(kind: Kind) -> OptionTable {
    match kind {
        Kind::Lint => OptionTable::Lint,
        Kind::Format => OptionTable::Format,
    }
}

/// One engine's `[<kind>.<lang>.<engine>]` table schema, from the keys it
/// declares.
fn engine_table(keys: &OptionKeys, universal: bool) -> Value {
    let mut properties = Map::new();
    for key in keys.known_keys(universal) {
        if let Some(ty) = keys.expected_type(key, universal)
            && let Some(schema) = type_schema(ty)
        {
            properties.insert(key.to_owned(), schema);
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": true
    })
}

/// A JSON Schema fragment for an [`OptionType`] bit set, or `None` for a type
/// with no JSON representation (a datetime).
fn type_schema(ty: OptionType) -> Option<Value> {
    let mut alternatives = Vec::new();
    if ty.accepts(&toml::Value::Boolean(true)) {
        alternatives.push(json!({ "type": "boolean" }));
    }
    // A float-declared key also accepts an integer (TOML widens `1` to `1.0`),
    // so emit one numeric alternative rather than a redundant `oneOf`.
    if ty.accepts(&toml::Value::Float(1.5)) {
        alternatives.push(json!({ "type": "number" }));
    } else if ty.accepts(&toml::Value::Integer(1)) {
        alternatives.push(json!({ "type": "integer" }));
    }
    if ty.accepts(&toml::Value::String(String::new())) {
        alternatives.push(json!({ "type": "string" }));
    }
    if ty.accepts(&toml::Value::Array(Vec::new())) {
        alternatives.push(json!({ "type": "array" }));
    }
    if ty.accepts(&toml::Value::Table(toml::Table::new())) {
        alternatives.push(json!({ "type": "object" }));
    }
    match alternatives.len() {
        0 => None,
        1 => alternatives.pop(),
        _ => Some(json!({ "oneOf": alternatives })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_carries_lint_and_fmt_with_declared_engine_keys() {
        let schema = poly_toml_schema();
        let ruff = &schema["properties"]["lint"]["properties"]["python"]["properties"]["ruff"];
        assert_eq!(ruff["$ref"], "#/$defs/lint_ruff", "{ruff}");
        let def = &schema["$defs"]["lint_ruff"];
        assert_eq!(def["properties"]["select"]["type"], "array", "{def}");
        assert_eq!(def["properties"]["enabled"]["type"], "boolean", "{def}");
        // The cross-cutting engines get a language-agnostic table too.
        assert!(schema["properties"]["lint"]["properties"]["typos"].is_object());
        // `fmt` reaches at least one per-language engine.
        assert_eq!(
            schema["properties"]["fmt"]["properties"]["toml"]["properties"]["taplo"]["$ref"],
            "#/$defs/fmt_taplo"
        );
    }

    #[test]
    fn engine_tables_are_deduplicated_into_defs() {
        let schema = poly_toml_schema();
        let defs = schema["$defs"].as_object().expect("$defs");
        // `ruff` is Python-only, but `typos` is cross-cutting and would have been
        // inlined under every language without the `$defs` hoist.
        assert_eq!(
            defs.keys().filter(|key| key.as_str() == "lint_typos").count(),
            1,
            "cross-cutting engine table is not deduplicated"
        );
        assert!(defs.contains_key("lint_cross_typos"), "{:?}", defs.keys());
    }
}
