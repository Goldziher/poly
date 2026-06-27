//! Insta snapshot fixtures for the typos spell-checker backend.
//!
//! - `known_bad_typo_diagnostics` — a plain-text file containing clear typos
//!   asserts the expected [`Diagnostic`]s (code `"typo"`, message with suggestion,
//!   1-based line/column span).
//! - `clean_file_has_no_typo_diagnostics` — verifies clean input produces no findings.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::typos::TyposEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn make_src(content: &str) -> SourceFile {
    SourceFile {
        path: "fixture.txt".into(),
        language: Language::Markdown,
        content: content.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: a file with several clear typos.
//
// "language" → "language"  (line 1)
// "receive"  → "receive"   (line 1)
// "the"      → "the"       (line 2)
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = "\
The language of the receive function.
This is the occurrence of a typo.
";

#[test]
fn known_bad_typo_diagnostics() {
    let engine = TyposEngine;
    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(
        !diags.is_empty(),
        "expected spell-check diagnostics for known-bad file"
    );

    // Summarise to (engine, code, message, span_start) for a stable snapshot.
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.message.as_str(),
                d.span.as_ref().map(|s| (s.start_line, s.start_col)),
            )
        })
        .collect();
    insta::assert_debug_snapshot!("known_bad_typo_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Clean file: no diagnostics expected.
// ---------------------------------------------------------------------------

#[test]
fn clean_file_has_no_typo_diagnostics() {
    let engine = TyposEngine;
    let src = make_src("The language of the receive function.\n");
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "expected no diagnostics for clean file, got: {diags:?}",
    );
}
