//! Insta snapshot fixtures for the biome CSS lint backend.
//!
//! - `known_bad_diagnostics` — a CSS file with an unknown property asserts the
//!   expected [`Diagnostic`]s from the biome correctness rules.
//! - `valid_css_no_diagnostics` — a valid CSS rule has no diagnostics.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::biome_css::BiomeCssEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str, lang: Language) -> SourceFile {
    SourceFile {
        path: "fixture.css".into(),
        language: lang,
        content: content.into(),
    }
}

/// CSS with a misspelled property — biome fires
/// `lint/correctness/noUnknownProperty` on `colr`.
const KNOWN_BAD: &str = "a { colr: blue; }\n";

#[test]
fn known_bad_diagnostics() {
    let engine = BiomeCssEngine;
    let diags = engine.lint(&make_src(KNOWN_BAD, Language::Css), &engine_cfg()).unwrap();

    assert!(
        !diags.is_empty(),
        "expected at least one diagnostic for unknown CSS property `colr`"
    );

    let summary: Vec<_> = diags
        .iter()
        .map(|d| (d.engine.as_str(), d.code.as_deref().unwrap_or(""), d.span.is_some()))
        .collect();
    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

#[test]
fn valid_css_no_diagnostics() {
    let engine = BiomeCssEngine;
    let diags = engine
        .lint(&make_src("a { color: blue; }\n", Language::Css), &engine_cfg())
        .unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for valid CSS; got: {diags:#?}"
    );
}
