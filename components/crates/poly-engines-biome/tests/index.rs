//! The committed component index must describe exactly the engines this crate serves.
//!
//! `poly-core` no longer links biome, so its `component_index` drift test keeps the `biome` entry from
//! the committed index; this test is what checks that entry against the real engines.

use std::path::Path;

use poly_component_index::ComponentIndex;
use poly_engine_api::desc::engine_desc;
use poly_engines_biome::{BiomeCssEngine, BiomeGraphqlEngine};

#[test]
fn the_committed_index_describes_the_biome_engines() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/poly-component-index/data/index.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let index = ComponentIndex::parse(&text).expect("index.json parses");
    let component = index
        .component("biome")
        .expect("the index declares the biome component");

    let mut expected = vec![engine_desc(&BiomeCssEngine), engine_desc(&BiomeGraphqlEngine)];
    expected.sort_by(|a, b| (&a.name, &a.languages).cmp(&(&b.name, &b.languages)));
    let committed = serde_json::to_value(&component.engines).unwrap();
    let actual = serde_json::to_value(&expected).unwrap();
    assert!(
        committed == actual,
        "the biome entry of component-index/data/index.json drifted from the engines; update the \
         entry (see `engines_for` and `Engine::version`) and rerun this test"
    );
}
