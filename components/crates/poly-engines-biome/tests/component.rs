//! The real `biome` component, built by alef from this workspace and loaded from a `file://` dev
//! lock, against the same engines linked in-process.
//!
//! Runs only when `POLY_COMPONENT_DEV_LOCK` names a lock written by
//! `scripts/components-dev.sh biome`; otherwise it reports that it skipped and passes.

use std::path::PathBuf;
use std::sync::Arc;

use poly_component_host::{ComponentHost, HostOptions};
use poly_component_protocol::decode_describe;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, SourceFile};
use poly_engine_api::language::Language;
use poly_engines::PolyComponent;
use poly_engines_biome::{BiomeComponent, BiomeCssEngine, BiomeGraphqlEngine};

const DEV_LOCK_ENV: &str = "POLY_COMPONENT_DEV_LOCK";

const BAD_CSS: &str = "a { colr: blue; }\n";
const BAD_GRAPHQL: &str = "query { user { id } }\n";

fn cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn source(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language,
        content: Arc::from(content),
    }
}

#[test]
fn the_biome_component_lints_like_the_in_process_engines() {
    let Some(lock) = std::env::var(DEV_LOCK_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!("skipped: {DEV_LOCK_ENV} is not set; run `scripts/components-dev.sh biome` first");
        return;
    };
    let lock = std::fs::read_to_string(&lock).unwrap_or_else(|e| panic!("reading {lock}: {e}"));
    let cache = tempfile::tempdir().unwrap();
    let host = ComponentHost::new(HostOptions::new(lock, cache.path())).expect("host over the dev lock");

    let describe =
        decode_describe(&BiomeComponent::default().describe().unwrap()).expect("in-process describe decodes");
    assert_eq!(
        describe.engines.len(),
        2,
        "the biome component hosts the CSS and GraphQL engines"
    );

    let cases: [(&str, SourceFile, Box<dyn Engine>); 2] = [
        (
            "css",
            source("test.css", Language::Css, BAD_CSS),
            Box::new(BiomeCssEngine),
        ),
        (
            "graphql",
            source("test.graphql", Language::GraphQl, BAD_GRAPHQL),
            Box::new(BiomeGraphqlEngine),
        ),
    ];
    for (language, src, in_process) in cases {
        let wire = describe
            .engines
            .iter()
            .find(|engine| engine.languages.iter().any(|l| l == language))
            .unwrap_or_else(|| panic!("no wire engine serves {language}"))
            .clone();
        assert_eq!(wire.name, "biome");
        let engine = host
            .engine("biome", &wire, Some(describe.clone()))
            .expect("engine proxy over the component");

        let expected = in_process.lint(&src, &cfg()).expect("in-process lint");
        let actual = engine.lint(&src, &cfg()).expect("component lint");
        assert!(!expected.is_empty(), "the {language} fixture must produce a finding");
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "{language} diagnostics differ across the component boundary"
        );
    }
}
