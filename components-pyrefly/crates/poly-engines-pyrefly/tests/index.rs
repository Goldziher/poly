//! The committed component index entry for `pyrefly` equals what this crate's engine describes,
//! since `poly-core` cannot link it (pyrefly lives in this workspace).
//!
//! Refresh it with `UPDATE_INDEX=1 cargo test --manifest-path components-pyrefly/Cargo.toml
//! -p poly-engines-pyrefly --test index`, then `poly fmt --fix` the file.

use poly_component_index::ComponentIndex;
use poly_engine_api::desc::engine_desc;
use poly_engines_pyrefly::PyreflyEngine;

#[test]
fn the_committed_index_describes_the_pyrefly_component() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/poly-component-index/data/index.json");
    let text = std::fs::read_to_string(&path).expect("index.json is committed");
    let mut index = ComponentIndex::parse(&text).expect("index.json parses");
    let described = vec![engine_desc(&PyreflyEngine)];
    if std::env::var_os("UPDATE_INDEX").is_some() {
        index
            .components
            .iter_mut()
            .find(|c| c.name == "pyrefly")
            .expect("the index declares the pyrefly component")
            .engines = described;
        let mut rendered = serde_json::to_string_pretty(&index).expect("index serializes");
        rendered.push('\n');
        std::fs::write(&path, rendered).expect("write index.json");
        return;
    }
    let component = index
        .component("pyrefly")
        .expect("the index declares the pyrefly component");
    assert!(component.opt_in, "pyrefly is opt-in: ty is the default type checker");
    assert!(
        component.download_only,
        "pyrefly lives outside `components/`, so no bundled build links it"
    );
    assert_eq!(
        component.engines, described,
        "the committed index is stale for pyrefly; regenerate it with \
         `UPDATE_INDEX=1 cargo test --manifest-path components-pyrefly/Cargo.toml -p poly-engines-pyrefly --test index`, \
         then `poly fmt --fix` the file"
    );
}
