//! Insta snapshot fixtures for the GraphQL backend.
//!
//! - `known_bad_diagnostics` — a malformed GraphQL document asserts the
//!   expected parse-error [`Diagnostic`].
//! - `known_unformatted_query_output` — a messy query is canonicalized.
//! - `known_unformatted_schema_output` — a messy SDL schema is canonicalized.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::graphql::GraphQlEngine,
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
        content: content.to_string(),
    }
}

fn format_to_string(content: &str) -> String {
    let engine = GraphQlEngine;
    match engine.format(&make_src(content), &engine_cfg()).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: a malformed document produces a parse diagnostic.
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = "query { user(id: ) { name } }";

#[test]
fn known_bad_diagnostics() {
    let engine = GraphQlEngine;
    let diags = engine.lint(&make_src(KNOWN_BAD), &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected a parse-error diagnostic");
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.span.is_some(),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

#[test]
fn valid_query_has_no_diagnostics() {
    let engine = GraphQlEngine;
    let diags = engine
        .lint(&make_src("query { user { id name } }"), &engine_cfg())
        .unwrap();
    assert!(diags.is_empty(), "got: {diags:?}");
}

// ---------------------------------------------------------------------------
// Known-unformatted fixtures: messy query / schema → canonical output.
// ---------------------------------------------------------------------------

const KNOWN_UNFORMATTED_QUERY: &str = "query   GetUser{user(id:42){name,email,posts{title}}}";

#[test]
fn known_unformatted_query_output() {
    insta::assert_snapshot!(
        "known_unformatted_query_output",
        format_to_string(KNOWN_UNFORMATTED_QUERY)
    );
}

const KNOWN_UNFORMATTED_SCHEMA: &str =
    "type   User{id:ID! name:String email:String posts:[Post!]!}\ntype Post{title:String}";

#[test]
fn known_unformatted_schema_output() {
    insta::assert_snapshot!(
        "known_unformatted_schema_output",
        format_to_string(KNOWN_UNFORMATTED_SCHEMA)
    );
}
