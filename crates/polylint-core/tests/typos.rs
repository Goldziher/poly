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
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: two short lines holding four common misspellings, each
// with a single dictionary correction. The content lives in an external
// fixture file (under tests/fixtures/, which `.typos.toml` excludes) so the
// `typos` pre-commit hook cannot "correct" the misspellings out of the file and
// silently break this test. Never inline a single-correction misspelling in
// this source for the same reason — assert structurally instead.
// ---------------------------------------------------------------------------

const KNOWN_BAD: &str = include_str!("fixtures/typos/known_bad.txt");

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

// ---------------------------------------------------------------------------
// Noise suppression: minified/generated assets and ultra-short tokens. These
// guards removed ~99% of false positives on the dry-run corpus (a minified
// 11.7 MB bundle that flagged every 2-char identifier).
// ---------------------------------------------------------------------------

#[test]
fn skips_minified_content_with_very_long_lines() {
    let engine = TyposEngine;
    // KNOWN_BAD has real typos on short lines (flagged above); collapsed onto a
    // single very long line it reads as minified and must be skipped entirely.
    let minified = KNOWN_BAD.replace('\n', " ").repeat(60);
    assert!(minified.len() > 2_000, "fixture must exceed the line guard");
    let diags = engine.lint(&make_src(&minified), &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "minified content must not be spell-checked, got: {diags:?}",
    );
}

#[test]
fn skips_oversized_content() {
    let engine = TyposEngine;
    // Same typos, but as a > 1 MiB file (short lines): the size guard skips it.
    let big = KNOWN_BAD.repeat(40_000);
    assert!(big.len() > (1 << 20), "fixture must exceed the size guard");
    let diags = engine.lint(&make_src(&big), &engine_cfg()).unwrap();
    assert!(
        diags.is_empty(),
        "oversized content must not be spell-checked, got {} diagnostics",
        diags.len(),
    );
}

#[test]
fn drops_ultra_short_corrections_but_keeps_three_char_typos() {
    let engine = TyposEngine;
    // short_tokens.txt holds a 2-char token (a typos-dict correction) followed
    // by a 3-char one. The 2-char token is dropped as too short to be reliable;
    // the 3-char one survives with its single-correction autofix. Assert
    // structurally so no misspelling literal lives in this source.
    let src = make_src(include_str!("fixtures/typos/short_tokens.txt"));
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert_eq!(
        diags.len(),
        1,
        "only the 3-char typo should survive the length filter: {diags:?}",
    );
    assert_eq!(
        diags[0].fix.first().map(|e| e.replacement.as_str()),
        Some("the"),
        "the surviving typo should carry its single-correction autofix",
    );
}
