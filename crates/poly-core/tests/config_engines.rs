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

use poly_core::config::{Config, Kind};
use poly_core::engine::{Engine, SourceFile};
use poly_core::engines::graphql::GraphQlEngine;
use poly_core::engines::sqruff::SqruffEngine;
use poly_core::engines::typos::TyposEngine;
use poly_core::language::Language;

// sqruff: exclude_rules silences the named rule

fn sqruff_src() -> SourceFile {
    SourceFile {
        path: PathBuf::from("check.sql"),
        language: Language::Sql,
        content: "select id, name FROM users\n".into(),
    }
}

#[test]
fn sqruff_honors_exclude_rules_option() {
    let engine = SqruffEngine;

    let default_cfg = Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint);
    let default_diags = engine.lint(&sqruff_src(), &default_cfg).unwrap();
    assert!(
        default_diags.iter().any(|d| d.code.as_deref() == Some("CP01")),
        "expected CP01 to fire on mixed-case SQL with default config; got: {default_diags:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("poly.toml");
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

/// The 3-char typo from `short_tokens.txt`, constructed from individual chars
/// so that the literal misspelling does not appear in this .rs source file
/// (the typos pre-commit hook scans all .rs files outside tests/fixtures/).
fn three_char_typo_word() -> String {
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

fn typos_default_cfg() -> poly_core::config::EngineConfig {
    poly_core::config::EngineConfig {
        globals: poly_core::config::GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

#[test]
fn typos_honors_extend_ignore_words_option() {
    let engine = TyposEngine;

    let default_diags = engine.lint(&typos_src(), &typos_default_cfg()).unwrap();
    assert_eq!(
        default_diags.len(),
        1,
        "short_tokens.txt should produce exactly 1 typo diagnostic with default config; \
         got: {default_diags:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("poly.toml");
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

#[test]
fn typos_honors_native_typos_config_file() {
    let engine = TyposEngine;

    let src = typos_src();
    let default_diags = engine.lint(&src, &typos_default_cfg()).unwrap();
    assert_eq!(
        default_diags.len(),
        1,
        "expected 1 diagnostic with default config for setup; got: {default_diags:?}",
    );

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

#[test]
fn typos_poly_toml_augments_native_config() {
    let engine = TyposEngine;

    const KNOWN_BAD: &str = include_str!("fixtures/typos/known_bad.txt");
    let src = SourceFile {
        path: PathBuf::from("doc.txt"),
        language: Language::Markdown,
        content: KNOWN_BAD.into(),
    };

    let default_diags = engine.lint(&src, &typos_default_cfg()).unwrap();
    assert!(
        !default_diags.is_empty(),
        "known_bad.txt must produce diagnostics with default config; got none",
    );

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

    let default_cfg = Config::default().engine_config(&Language::GraphQl, "graphql", Kind::Format);
    let default_out = engine.format(&graphql_src(), &default_cfg).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("poly.toml");
    fs::write(&toml_path, "[fmt.graphql.graphql]\nindent_width = 4\n").unwrap();
    let cfg4 = Config::load_file(&toml_path)
        .unwrap()
        .engine_config(&Language::GraphQl, "graphql", Kind::Format);
    let poly_core::engine::FormatOutput::Formatted(text4) = engine.format(&graphql_src(), &cfg4).unwrap() else {
        panic!("indent_width = 4 must reformat the 2-space input; got Unchanged");
    };
    assert!(
        text4.lines().any(|l| l.starts_with("    ") && !l.starts_with("     ")),
        "expected a 4-space-indented line in output: {text4:?}"
    );

    if let poly_core::engine::FormatOutput::Formatted(ref text2) = default_out {
        assert!(
            !text2.lines().any(|l| l.starts_with("    ") && !l.starts_with("     ")),
            "default (2-space) output must not use 4-space indentation: {text2:?}"
        );
    }
}
