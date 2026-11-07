//! The host builds the uncomment component's options out of `poly.toml` in every build flavour, so a
//! core build (no `engine-uncomment`) hands the component the same merged keys the bundled engine gets.
//! No feature gate: this runs under `--no-default-features --features core`, the shipped flavour.

use poly_core::config::{Config, Kind};
use poly_core::language::Language;

#[test]
fn global_and_language_uncomment_options_reach_the_engine_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    std::fs::write(
        &path,
        "[lint.uncomment]\nenabled = true\nremove_docs = true\npreserve_patterns = [\"SAFETY:\"]\n\
         [lint.python.uncomment]\npreserve_patterns = [\"LICENSE\"]\n",
    )
    .unwrap();
    let config = Config::load_file(&path).expect("load");

    let options = config.engine_config(&Language::Rust, "uncomment", Kind::Lint).options;
    let patterns: Vec<&str> = options["preserve_patterns"]
        .as_array()
        .expect("preserve_patterns array")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(patterns, ["SAFETY:"]);
    assert_eq!(options["remove_docs"].as_bool(), Some(true));

    let python = config.engine_config(&Language::Python, "uncomment", Kind::Lint).options;
    let patterns: Vec<&str> = python["preserve_patterns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(patterns, ["SAFETY:", "LICENSE"]);
}
