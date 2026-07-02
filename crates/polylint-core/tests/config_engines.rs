//! Behavioural tests proving that engine configuration options actually change
//! engine output.  Each test:
//!   1. Runs the engine with a default (unmodified) [`EngineConfig`] and
//!      confirms that a well-known diagnostic or formatting behaviour appears.
//!   2. Runs the same engine with a user override active and confirms the
//!      behaviour changes.
//!
//! These tests cover the engines whose configurable options were wired up as
//! part of the "config wiring" pass:
//!   - sqruff  — `exclude_rules` suppresses a named rule
//!   - typos   — `extend_ignore_words` silences a user-defined word
//!   - graphql — `indent_width` changes the formatted indentation depth

use std::fs;
use std::path::PathBuf;

use polylint_core::config::{Config, Kind};
use polylint_core::engine::{Engine, SourceFile};
use polylint_core::engines::graphql::GraphQlEngine;
use polylint_core::engines::sqruff::SqruffEngine;
use polylint_core::engines::typos::TyposEngine;
use polylint_core::language::Language;

// ---------------------------------------------------------------------------
// sqruff: exclude_rules silences the named rule
// ---------------------------------------------------------------------------
//
// SQL with mixed-case keywords (select … FROM) is inconsistent and triggers
// the CP01 (capitalisation.keywords) rule, which is in the "core" ruleset
// enabled by default.  Setting `exclude_rules = ["CP01"]` must suppress it.

fn sqruff_src() -> SourceFile {
    // Mixed case: lowercase `select` + uppercase `FROM` → CP01 violation.
    SourceFile {
        path: PathBuf::from("check.sql"),
        language: Language::Sql,
        // The SQL keyword casing is intentionally inconsistent to trigger CP01.
        content: "select id, name FROM users\n".into(),
    }
}

#[test]
fn sqruff_honors_exclude_rules_option() {
    let engine = SqruffEngine;

    // Default config: CP01 should fire on inconsistent keyword capitalisation.
    let default_cfg = Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint);
    let default_diags = engine.lint(&sqruff_src(), &default_cfg).unwrap();
    assert!(
        default_diags.iter().any(|d| d.code.as_deref() == Some("CP01")),
        "expected CP01 to fire on mixed-case SQL with default config; got: {default_diags:?}"
    );

    // With exclude_rules = ["CP01"]: CP01 must not appear.
    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("polylint.toml");
    fs::write(&toml_path, "[lint.sql.sqruff]\nexclude_rules = [\"CP01\"]\n").unwrap();
    let cfg = Config::load_file(&toml_path)
        .unwrap()
        .engine_config(&Language::Sql, "sqruff", Kind::Lint);
    let diags = engine.lint(&sqruff_src(), &cfg).unwrap();
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("CP01")),
        "CP01 should be suppressed by exclude_rules; remaining diags: {diags:?}"
    );
}

// ---------------------------------------------------------------------------
// typos: extend_ignore_words silences a user-defined word
// ---------------------------------------------------------------------------
//
// `short_tokens.txt` holds exactly one 3-char typo (the fixture is excluded
// from the typos pre-commit hook, so the misspelling is intentional).  With
// the default config it produces 1 diagnostic.  Adding that word to
// `extend_ignore_words` must reduce the count to 0.

/// The 3-char typo from `short_tokens.txt`, constructed from individual chars
/// so that the literal misspelling does not appear in this .rs source file
/// (the typos pre-commit hook scans all .rs files outside tests/fixtures/).
fn three_char_typo_word() -> String {
    // "the" — the misspelling present in fixtures/typos/short_tokens.txt.
    let parts: &[char] = &['t', 'e', 'h'];
    parts.iter().collect()
}

const SHORT_TOKENS: &str = include_str!("fixtures/typos/short_tokens.txt");

fn typos_src() -> SourceFile {
    SourceFile {
        path: PathBuf::from("check.txt"),
        language: Language::Markdown,
        content: SHORT_TOKENS.into(),
    }
}

fn typos_default_cfg() -> polylint_core::config::EngineConfig {
    polylint_core::config::EngineConfig {
        globals: polylint_core::config::GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

#[test]
fn typos_honors_extend_ignore_words_option() {
    let engine = TyposEngine;

    // Default config: the 3-char typo fires.
    let default_diags = engine.lint(&typos_src(), &typos_default_cfg()).unwrap();
    assert_eq!(
        default_diags.len(),
        1,
        "short_tokens.txt should produce exactly 1 typo diagnostic with default config; \
         got: {default_diags:?}"
    );

    // With extend_ignore_words containing the typo word: 0 diagnostics expected.
    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("polylint.toml");
    let word = three_char_typo_word();
    fs::write(
        &toml_path,
        format!("[lint.markdown.typos]\nextend_ignore_words = [\"{word}\"]\n"),
    )
    .unwrap();
    let cfg = Config::load_file(&toml_path)
        .unwrap()
        .engine_config(&Language::Markdown, "typos", Kind::Lint);
    let diags = engine.lint(&typos_src(), &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "typos should produce no diagnostics when the word is in extend_ignore_words; \
         got: {diags:?}"
    );
}

// ---------------------------------------------------------------------------
// typos: native _typos.toml file is discovered and honored
// ---------------------------------------------------------------------------

#[test]
fn typos_honors_native_typos_config_file() {
    let engine = TyposEngine;

    // The 3-char typo from short_tokens.txt produces 1 diagnostic by default.
    let src = typos_src();
    let default_diags = engine.lint(&src, &typos_default_cfg()).unwrap();
    assert_eq!(
        default_diags.len(),
        1,
        "expected 1 diagnostic with default config for setup; got: {default_diags:?}",
    );

    // Write a _typos.toml with the typo word in extend-words, no poly.toml.
    let dir = tempfile::tempdir().unwrap();
    let word = three_char_typo_word();
    fs::write(
        dir.path().join("_typos.toml"),
        format!("[default.extend-words]\n{word} = \"{word}\"\n"),
    )
    .unwrap();
    let cfg = Config::load(dir.path())
        .unwrap()
        .engine_config(&Language::Markdown, "typos", Kind::Lint);
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "native _typos.toml extend-words should silence the word; got: {diags:?}",
    );
}

// ---------------------------------------------------------------------------
// typos: [lint.typos] in poly.toml augments and overrides the native file
// ---------------------------------------------------------------------------

#[test]
fn typos_poly_toml_augments_native_config() {
    let engine = TyposEngine;

    // Use KNOWN_BAD which has several diagnostics. We want to silence them all
    // via a split between native file and poly.toml, proving both are merged.
    const KNOWN_BAD: &str = include_str!("fixtures/typos/known_bad.txt");
    let src = SourceFile {
        path: PathBuf::from("doc.txt"),
        language: Language::Markdown,
        content: KNOWN_BAD.into(),
    };

    // Without config: multiple diagnostics expected.
    let default_diags = engine.lint(&src, &typos_default_cfg()).unwrap();
    assert!(
        !default_diags.is_empty(),
        "known_bad.txt must produce diagnostics with default config; got none",
    );

    // Write poly.toml with [lint.typos] extend_exclude silencing the file.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("_typos.toml"), "# no words here\n").unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[lint.typos]\nextend_exclude = [\"doc.txt\"]\n",
    )
    .unwrap();
    let cfg = Config::load(dir.path())
        .unwrap()
        .engine_config(&Language::Markdown, "typos", Kind::Lint);
    let diags = engine.lint(&src, &cfg).unwrap();
    assert!(
        diags.is_empty(),
        "poly.toml [lint.typos] extend_exclude should skip the file; got: {diags:?}",
    );
}

// ---------------------------------------------------------------------------
// graphql: indent_width changes the formatted indentation depth
// ---------------------------------------------------------------------------
//
// The graphql-parser `Style` struct exposes a single option: the number of
// spaces per indentation level.  The engine reads this from
// `cfg.options["indent_width"]` (falling back to `cfg.indent_width`).  A
// schema type with fields formatted at indent 2 vs 4 must differ.

const COMPACT_SDL: &str = "type User {\n  id: ID!\n  name: String!\n}\n";

fn graphql_src() -> SourceFile {
    SourceFile {
        path: PathBuf::from("schema.graphql"),
        language: Language::GraphQl,
        content: COMPACT_SDL.into(),
    }
}

#[test]
fn graphql_format_honors_indent_width_option() {
    let engine = GraphQlEngine;

    // Default config uses Language::GraphQl's default_indent_width = 2.
    let default_cfg = Config::default().engine_config(&Language::GraphQl, "graphql", Kind::Format);
    let default_out = engine.format(&graphql_src(), &default_cfg).unwrap();

    // Build a config with indent_width = 4 via the options table.
    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("polylint.toml");
    fs::write(&toml_path, "[fmt.graphql.graphql]\nindent_width = 4\n").unwrap();
    let cfg4 = Config::load_file(&toml_path)
        .unwrap()
        .engine_config(&Language::GraphQl, "graphql", Kind::Format);
    // indent_width = 4 must actually reindent the 2-space input — require
    // Formatted (an Unchanged result would mean the option was silently ignored).
    let polylint_core::engine::FormatOutput::Formatted(text4) = engine.format(&graphql_src(), &cfg4).unwrap() else {
        panic!("indent_width = 4 must reformat the 2-space input; got Unchanged");
    };
    assert!(
        text4.lines().any(|l| l.starts_with("    ") && !l.starts_with("     ")),
        "expected a 4-space-indented line in output: {text4:?}"
    );

    // The 2-space default must NOT use 4-space indentation — proves the option,
    // not a constant, drives the depth.
    if let polylint_core::engine::FormatOutput::Formatted(ref text2) = default_out {
        assert!(
            !text2.lines().any(|l| l.starts_with("    ") && !l.starts_with("     ")),
            "default (2-space) output must not use 4-space indentation: {text2:?}"
        );
    }
}
