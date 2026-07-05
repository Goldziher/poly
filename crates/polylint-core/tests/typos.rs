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

    assert!(!diags.is_empty(), "expected spell-check diagnostics for known-bad file");

    // Summarise to (engine, code, message, span_start) for a stable snapshot.
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.engine.as_str(),
                d.code.as_deref().unwrap_or(""),
                d.title.as_str(),
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
    // the 3-char one survives. Assert structurally so no misspelling literal
    // lives in this source.
    let src = make_src(include_str!("fixtures/typos/short_tokens.txt"));
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert_eq!(
        diags.len(),
        1,
        "only the 3-char typo should survive the length filter: {diags:?}",
    );
    // Typos are reported at error severity with the suggestion in the message,
    // and never carry an autofix (manual resolution required).
    assert_eq!(
        diags[0].severity,
        polylint_core::engine::Severity::Error,
        "typos must be error severity",
    );
    assert!(diags[0].fix.is_empty(), "typos must not carry an autofix");
    assert!(
        diags[0].title.contains("the"),
        "the surviving typo should suggest `the` in its message: {}",
        diags[0].title,
    );
}

// ---------------------------------------------------------------------------
// Built-in allow-list: universally-correct technical terms and OSS names are
// valid with no per-repo config (e.g. GPG `fpr`, the `certifi` package).
// ---------------------------------------------------------------------------

#[test]
fn builtin_valid_words_are_not_flagged() {
    let engine = TyposEngine;
    // `fpr` (GPG fingerprint) is flagged by the built-in dictionary by default;
    // the engine's built-in allow-list must silence it and the other baked-in
    // terms without any configuration.
    for term in ["fpr", "certifi", "ser", "flate", "onnx"] {
        let src = make_src(&format!("the {term} value here\n"));
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "built-in valid word `{term}` must not be flagged, got: {diags:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// extend_words: a word in the map is treated as a valid spelling.
// ---------------------------------------------------------------------------

const SHORT_TOKENS: &str = include_str!("fixtures/typos/short_tokens.txt");

#[test]
fn extend_words_silences_configured_word() {
    // short_tokens.txt contains a single 3-char typo (teh → the). Adding its
    // key to extend_words must reduce the diagnostic count to zero.
    let engine = TyposEngine;
    let src = make_src(SHORT_TOKENS);
    // Build options with extend_words containing the typo word.
    // The word is constructed from chars so the literal misspelling does not
    // appear in this source file.
    let typo_word: String = ['t', 'e', 'h'].iter().collect();
    let mut words_table = toml::Table::new();
    words_table.insert(typo_word.clone(), toml::Value::String(typo_word));
    let mut options = toml::Table::new();
    options.insert("extend_words".to_string(), toml::Value::Table(words_table));
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "extend_words should silence the configured word; got: {diags:?}",
    );
}

// ---------------------------------------------------------------------------
// extend_identifiers: a compound identifier is treated as valid, suppressing
// any word-level typo flagging within it.
// ---------------------------------------------------------------------------

const IDENTIFIER_WITH_TYPO: &str = include_str!("fixtures/typos/identifier_with_typo.txt");

#[test]
fn extend_identifiers_silences_configured_identifier() {
    let engine = TyposEngine;
    // Without any config, the fixture produces at least one diagnostic (the
    // word token within the identifier is a known typo).
    let src_flagged = SourceFile {
        path: "fixture.txt".into(),
        language: Language::Markdown,
        content: IDENTIFIER_WITH_TYPO.into(),
    };
    let default_diags = engine.lint(&src_flagged, &engine_cfg()).unwrap();
    assert!(
        !default_diags.is_empty(),
        "expected diagnostics for identifier_with_typo.txt with default config; got none",
    );

    // Adding the identifier to extend_identifiers must silence the diagnostic.
    let ident: String = ['t', 'e', 'h', '_', 'v', 'a', 'r', 'i', 'a', 'b', 'l', 'e']
        .iter()
        .collect();
    let mut idents_table = toml::Table::new();
    idents_table.insert(ident.clone(), toml::Value::String(ident));
    let mut options = toml::Table::new();
    options.insert("extend_identifiers".to_string(), toml::Value::Table(idents_table));
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };
    let diags = engine.lint(&src_flagged, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "extend_identifiers should silence the identifier; got: {diags:?}",
    );
}

// ---------------------------------------------------------------------------
// extend_exclude: files whose path matches a glob are skipped entirely.
// ---------------------------------------------------------------------------

#[test]
fn extend_exclude_skips_matching_file() {
    let engine = TyposEngine;
    // The known-bad fixture produces diagnostics normally.
    let src = SourceFile {
        path: "tests/fixtures/typos/known_bad.txt".into(),
        language: Language::Markdown,
        content: KNOWN_BAD.into(),
    };
    let default_diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert!(
        !default_diags.is_empty(),
        "expected diagnostics before excluding; got none"
    );

    // With extend_exclude matching the file's path, it must be skipped.
    let mut options = toml::Table::new();
    options.insert(
        "extend_exclude".to_string(),
        toml::Value::Array(vec![toml::Value::String("tests/fixtures/**".to_string())]),
    );
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    };
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "extend_exclude should skip the matched file; got: {diags:?}",
    );
}

// ---------------------------------------------------------------------------
// extend_ignore_re: a regex masks a region of the file; typos inside the
// matched span are dropped, typos elsewhere still fire (typos-cli semantics).
// ---------------------------------------------------------------------------

/// Titles of the typos flagged for KNOWN_BAD, so tests can assert which
/// misspellings survive a filter without inlining the misspellings themselves.
fn flagged_words(cfg: &EngineConfig) -> Vec<String> {
    TyposEngine
        .lint(&make_src(KNOWN_BAD), cfg)
        .unwrap()
        .iter()
        .map(|d| d.title.clone())
        .collect()
}

fn options_with_string_array(key: &str, values: &[&str]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert(
        key.to_string(),
        toml::Value::Array(values.iter().map(|s| toml::Value::String((*s).to_string())).collect()),
    );
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options,
    }
}

#[test]
fn extend_ignore_re_masks_a_region() {
    // KNOWN_BAD line 2 reads "This is teh occurence of a typo." — a regex that
    // matches that region must drop both "teh" and "occurence" while leaving the
    // line-1 typos ("languge", "recieve") intact.
    let baseline = flagged_words(&engine_cfg());
    assert_eq!(baseline.len(), 4, "KNOWN_BAD has four typos by default: {baseline:?}");

    // The masked substring is assembled from chars so no misspelling literal
    // lives in this source (the typos pre-commit hook rewrites those).
    let masked: String = ['t', 'e', 'h', ' ', 'o', 'c', 'c', 'u', 'r', 'e', 'n', 'c', 'e']
        .iter()
        .collect();
    let cfg = options_with_string_array("extend_ignore_re", &[&masked]);
    let survived = flagged_words(&cfg);
    assert_eq!(
        survived.len(),
        2,
        "region mask should drop the two typos inside it: {survived:?}",
    );
    let teh: String = ['`', 't', 'e', 'h', '`'].iter().collect();
    assert!(
        !survived.iter().any(|t| t.contains(&teh)),
        "masked typo must not survive: {survived:?}",
    );
}

// ---------------------------------------------------------------------------
// extend_ignore_words_re: a regex matching the flagged word drops just that
// word; unrelated typos remain.
// ---------------------------------------------------------------------------

#[test]
fn extend_ignore_words_re_drops_only_matching_word() {
    let baseline = flagged_words(&engine_cfg());
    assert_eq!(baseline.len(), 4, "sanity: four typos: {baseline:?}");

    // Match exactly the 3-char typo word (built from chars to avoid the literal).
    let pattern: String = ['^', 't', 'e', 'h', '$'].iter().collect();
    let cfg = options_with_string_array("extend_ignore_words_re", &[&pattern]);
    let survived = flagged_words(&cfg);
    assert_eq!(
        survived.len(),
        3,
        "only the word-regex match should be dropped: {survived:?}",
    );
}
