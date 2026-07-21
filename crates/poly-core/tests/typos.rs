//! Insta snapshot fixtures for the typos spell-checker backend.
//!
//! - `known_bad_typo_diagnostics` — a plain-text file containing clear typos
//!   asserts the expected [`Diagnostic`]s (code `"typo"`, message with suggestion,
//!   1-based line/column span).
//! - `clean_file_has_no_typo_diagnostics` — verifies clean input produces no findings.

use poly_core::{
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

const KNOWN_BAD: &str = include_str!("fixtures/typos/known_bad.txt");

#[test]
fn known_bad_typo_diagnostics() {
    let engine = TyposEngine;
    let src = make_src(KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    assert!(!diags.is_empty(), "expected spell-check diagnostics for known-bad file");

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

#[test]
fn skips_minified_content_with_very_long_lines() {
    let engine = TyposEngine;
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
    let src = make_src(include_str!("fixtures/typos/short_tokens.txt"));
    let diags = engine.lint(&src, &engine_cfg()).unwrap();
    assert_eq!(
        diags.len(),
        1,
        "only the 3-char typo should survive the length filter: {diags:?}",
    );
    assert_eq!(
        diags[0].severity,
        poly_core::engine::Severity::Warning,
        "typos must be warning severity so a false positive never fails CI",
    );
    assert!(diags[0].fix.is_empty(), "typos must not carry an autofix");
    assert!(
        diags[0].title.contains("the"),
        "the surviving typo should suggest `the` in its message: {}",
        diags[0].title,
    );
}

#[test]
fn builtin_valid_words_are_not_flagged() {
    let engine = TyposEngine;
    for term in ["fpr", "certifi", "ser", "flate", "onnx"] {
        let src = make_src(&format!("the {term} value here\n"));
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "built-in valid word `{term}` must not be flagged, got: {diags:?}",
        );
    }
}

const SHORT_TOKENS: &str = include_str!("fixtures/typos/short_tokens.txt");

#[test]
fn extend_words_silences_configured_word() {
    let engine = TyposEngine;
    let src = make_src(SHORT_TOKENS);
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

const IDENTIFIER_WITH_TYPO: &str = include_str!("fixtures/typos/identifier_with_typo.txt");

#[test]
fn extend_identifiers_silences_configured_identifier() {
    let engine = TyposEngine;
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

#[test]
fn extend_exclude_skips_matching_file() {
    let engine = TyposEngine;
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
    let baseline = flagged_words(&engine_cfg());
    assert_eq!(baseline.len(), 4, "KNOWN_BAD has four typos by default: {baseline:?}");

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

#[test]
fn extend_ignore_words_re_drops_only_matching_word() {
    let baseline = flagged_words(&engine_cfg());
    assert_eq!(baseline.len(), 4, "sanity: four typos: {baseline:?}");

    let pattern: String = ['^', 't', 'e', 'h', '$'].iter().collect();
    let cfg = options_with_string_array("extend_ignore_words_re", &[&pattern]);
    let survived = flagged_words(&cfg);
    assert_eq!(
        survived.len(),
        3,
        "only the word-regex match should be dropped: {survived:?}",
    );
}
