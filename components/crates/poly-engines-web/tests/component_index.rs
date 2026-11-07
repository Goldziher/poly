//! The committed component index entry for `web` equals what this crate's engines describe, since
//! `poly-core` cannot link them (its own `tests/component_index.rs` carries the entry over).
//!
//! Refresh it with
//! `UPDATE_INDEX=1 cargo test --manifest-path components/Cargo.toml -p poly-engines-web --test component_index`,
//! then `poly fmt --fix` the file.

use poly_component_index::ComponentIndex;
use poly_component_protocol::WireEngineDesc;
use poly_engine_api::desc::engine_desc;
use poly_engine_api::language::Language;
use poly_engines_web::engines_for;

/// Languages whose engines together cover every web engine.
const LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::Toml,
    Language::Markdown,
    Language::Mdx,
    Language::Yaml,
    Language::Css,
    Language::Html,
];

/// The wire description of every web engine, once each, in index order.
fn described() -> Vec<WireEngineDesc> {
    let mut descs: Vec<WireEngineDesc> = Vec::new();
    for language in LANGUAGES {
        for engine in engines_for(language) {
            let desc = engine_desc(engine.as_ref());
            if !descs.contains(&desc) {
                descs.push(desc);
            }
        }
    }
    descs.sort_by(|a, b| (&a.name, &a.languages).cmp(&(&b.name, &b.languages)));
    descs
}

#[test]
fn the_provider_serves_the_web_languages_only() {
    assert_eq!(described().len(), 8, "every web engine is reached by a sample language");
    assert!(engines_for(&Language::Python).is_empty());
    assert!(engines_for(&Language::Ruby).is_empty());
    let toml: Vec<_> = engines_for(&Language::Toml).iter().map(|e| e.name()).collect();
    assert_eq!(
        toml,
        ["taplo", "skif"],
        "poly.toml key checking stays a core engine; skif reads skif.toml"
    );
}

#[test]
fn the_committed_index_describes_the_web_component() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/poly-component-index/data/index.json");
    let text = std::fs::read_to_string(&path).expect("index.json is committed");
    let mut index = ComponentIndex::parse(&text).expect("index.json parses");
    let described = described();
    if std::env::var_os("UPDATE_INDEX").is_some() {
        index
            .components
            .iter_mut()
            .find(|c| c.name == "web")
            .expect("the index declares the web component")
            .engines = described;
        let mut rendered = serde_json::to_string_pretty(&index).expect("index serializes");
        rendered.push('\n');
        std::fs::write(&path, rendered).expect("write index.json");
        return;
    }
    let component = index.component("web").expect("the index declares the web component");
    assert_eq!(
        component.engines, described,
        "the committed index is stale for web; regenerate it with \
         `UPDATE_INDEX=1 cargo test --manifest-path components/Cargo.toml -p poly-engines-web --test component_index`, \
         then `poly fmt --fix` the file"
    );
}
