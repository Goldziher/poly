//! Insta snapshot fixtures for the biome GraphQL lint backend.
//!
//! - `known_bad_diagnostics` — a GraphQL document with an anonymous operation
//!   asserts the expected [`Diagnostic`]s from the biome correctness rules.
//! - `valid_graphql_no_diagnostics` — a correctly-named operation has no
//!   diagnostics from the biome engine.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::biome_graphql::BiomeGraphqlEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: "fixture.graphql".into(),
        language: Language::GraphQl,
        content: content.into(),
    }
}

/// An anonymous GraphQL operation — biome fires
/// `lint/correctness/useGraphqlNamedOperations`.
const KNOWN_BAD: &str = "query { user { id } }\n";

#[test]
fn known_bad_diagnostics() {
    let engine = BiomeGraphqlEngine;
    let diags = engine.lint(&make_src(KNOWN_BAD), &engine_cfg()).unwrap();

    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic for anonymous operation"
    );

    let summary: Vec<_> = diags
        .iter()
        .map(|d| (d.engine.as_str(), d.code.as_deref().unwrap_or(""), d.span.is_some()))
        .collect();
    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

#[test]
fn valid_graphql_no_diagnostics() {
    let engine = BiomeGraphqlEngine;
    let diags = engine
        .lint(&make_src("query GetUser { user { id name } }\n"), &engine_cfg())
        .unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for valid named query; got: {diags:#?}"
    );
}
