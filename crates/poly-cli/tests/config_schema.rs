//! The published `poly.toml` JSON Schema must match what the binary generates.
//!
//! `website/public/poly.schema.json` is served for editor validation and
//! autocomplete, and referenced from the configuration guide. A change to any
//! config type, or to an engine's declared option keys, silently stales it —
//! this test is what makes that a build failure instead. Compare *parsed* JSON,
//! not bytes: the committed file is normalised by `poly fmt`, which need not
//! match `serde_json`'s pretty printer byte-for-byte.

/// Path of the published schema, relative to this crate.
const PUBLISHED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../website/public/poly.schema.json");

#[test]
fn published_config_schema_is_current() {
    let generated = poly_core::config_schema::poly_toml_schema();
    let text = std::fs::read_to_string(PUBLISHED).unwrap_or_else(|error| panic!("reading {PUBLISHED}: {error}"));
    let committed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("parsing {PUBLISHED}: {error}"));

    assert_eq!(
        generated, committed,
        "the published poly.toml schema is stale; regenerate it with \
         `poly config schema --output website/public/poly.schema.json` then `poly fmt --fix` \
         website/public/poly.schema.json"
    );
}
