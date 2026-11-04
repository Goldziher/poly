//! `engine_versions()` names the web family once the bundled binary registers it, the way
//! `poly-core`'s own `tests/engine_versions.rs` does for the engines it links.

mod common;

#[test]
fn it_names_the_web_backends_once_they_are_registered() {
    common::register();
    let versions = poly_core::engine_versions();
    for expected in ["oxc", "taplo", "rumdl", "adrlint", "yaml", "malva", "markup_fmt"] {
        assert!(
            versions.get(expected).is_some_and(|version| !version.is_empty()),
            "{expected} is registered and must be listed with a version: {:?}",
            versions.keys().collect::<Vec<_>>()
        );
    }
}
