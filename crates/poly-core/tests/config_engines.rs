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

/// An implicit table alias (`a b`), which trips AL02 — an *aliasing* rule, and
/// so lint-owned. The fixture deliberately avoids capitalisation and layout
/// violations: those groups are format-owned and suppressed from `lint`
/// entirely (see `engines::sqruff`), which would make an assertion about them
/// pass for the wrong reason.
fn sqruff_src() -> SourceFile {
    SourceFile {
        path: PathBuf::from("check.sql"),
        language: Language::Sql,
        content: "select a b from users\n".into(),
    }
}

#[test]
fn sqruff_honors_exclude_rules_option() {
    let engine = SqruffEngine;

    let default_cfg = Config::default().engine_config(&Language::Sql, "sqruff", Kind::Lint);
    let default_diags = engine.lint(&sqruff_src(), &default_cfg).unwrap();
    assert!(
        default_diags.iter().any(|d| d.code.as_deref() == Some("AL02")),
        "expected AL02 to fire on an implicit alias with default config; got: {default_diags:?}"
    );

    let dir = tempfile::tempdir().unwrap();
    let toml_path = dir.path().join("poly.toml");
    fs::write(&toml_path, "[lint.sql.sqruff]\nexclude_rules = [\"AL02\"]\n").unwrap();
    let cfg = Config::load_file(&toml_path)
        .unwrap()
        .engine_config(&Language::Sql, "sqruff", Kind::Lint);
    let diags = engine.lint(&sqruff_src(), &cfg).unwrap();
    assert!(
        !diags.iter().any(|d| d.code.as_deref() == Some("AL02")),
        "AL02 should be suppressed by exclude_rules; remaining diags: {diags:?}"
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

// ── regression cover for keys that parsed but were never consumed ──
//
// Each test below pins a config key that poly accepted without complaint and
// then discarded. They are grouped here rather than in the per-backend fixture
// files because the property under test is the *config contract*, not the
// backend's formatting or rule behaviour.

/// Load `contents` as a `poly.toml` and slice out one engine's config.
fn engine_cfg_from(contents: &str, language: &Language, engine: &str, kind: Kind) -> poly_core::config::EngineConfig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, contents).unwrap();
    Config::load_file(&path).unwrap().engine_config(language, engine, kind)
}

fn formatted(engine: &dyn Engine, src: &SourceFile, cfg: &poly_core::config::EngineConfig) -> String {
    match engine.format(src, cfg).unwrap() {
        poly_core::engine::FormatOutput::Formatted(text) => text,
        poly_core::engine::FormatOutput::Unchanged => src.content.to_string(),
    }
}

/// A Python docstring holding a badly-spaced doctest. With docstring code
/// formatting on (poly's default) the doctest is reformatted; `[fmt.python.ruff]
/// docstring_code_format = false` must switch that off — it used to be read
/// nowhere, so `format()` always ran with the default.
#[test]
fn ruff_format_honors_docstring_code_format_option() {
    let engine = poly_core::engines::ruff::RuffEngine;
    let src = SourceFile {
        path: PathBuf::from("mod.py"),
        language: Language::Python,
        content: "def f():\n    \"\"\"Doc.\n\n    >>> x   =   1+2\n    \"\"\"\n".into(),
    };

    let on = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Python, "ruff", Kind::Format),
    );
    assert!(
        on.contains(">>> x = 1 + 2"),
        "poly formats docstring code by default; got: {on:?}"
    );

    let off = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.python.ruff]\ndocstring_code_format = false\n",
            &Language::Python,
            "ruff",
            Kind::Format,
        ),
    );
    assert!(
        off.contains(">>> x   =   1+2"),
        "docstring_code_format = false must leave the doctest untouched; got: {off:?}"
    );
}

/// `[fmt.python.ruff] indent_width` reaches the ruff formatter. It resolves into
/// `EngineConfig::indent_width` for every engine, but ruff's `format` used to
/// ignore it and always emit four spaces.
#[test]
fn ruff_format_honors_indent_width_option() {
    let engine = poly_core::engines::ruff::RuffEngine;
    let src = SourceFile {
        path: PathBuf::from("mod.py"),
        language: Language::Python,
        content: "def f():\n        return 1\n".into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Python, "ruff", Kind::Format),
    );
    assert_eq!(default, "def f():\n    return 1\n", "ruff's default indent is 4 spaces");

    let narrow = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.python.ruff]\nindent_width = 2\n",
            &Language::Python,
            "ruff",
            Kind::Format,
        ),
    );
    assert_eq!(
        narrow, "def f():\n  return 1\n",
        "indent_width = 2 must reach the formatter"
    );
}

/// ADR 0016 promises that any non-`level` key under `[rules.<id>]` is passed to
/// the backend as a tool parameter. rumdl's MD044 (proper names) takes a `names`
/// list, and nothing fires without it — so a diagnostic appearing at all proves
/// the parameter was forwarded.
#[test]
fn rumdl_honors_canonical_per_rule_params() {
    let engine = poly_core::engines::rumdl::RumdlEngine;
    let src = SourceFile {
        path: PathBuf::from("doc.md"),
        language: Language::Markdown,
        content: "# Title\n\nI use javascript daily.\n".into(),
    };

    let default = engine
        .lint(&src, &engine_cfg_from("", &Language::Markdown, "rumdl", Kind::Lint))
        .unwrap();
    assert!(
        !default.iter().any(|d| d.code.as_deref() == Some("MD044")),
        "MD044 has no configured names by default, so it must not fire; got: {default:?}"
    );

    for id in ["MD044", "proper-names"] {
        let cfg = engine_cfg_from(
            &format!("[lint.markdown.rumdl.rules.{id}]\nnames = [\"JavaScript\"]\n"),
            &Language::Markdown,
            "rumdl",
            Kind::Lint,
        );
        let diags = engine.lint(&src, &cfg).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("MD044")),
            "[rules.{id}] names must reach rumdl and make MD044 fire; got: {diags:?}"
        );
    }
}

/// sqruff files per-rule config by rule *name*, so the canonical `[rules.<id>]`
/// form must accept both the name and the code a user is more likely to reach
/// for. Both used to be discarded; only the sqruff-native `rule_configs` worked.
#[test]
fn sqruff_honors_canonical_per_rule_params() {
    let engine = SqruffEngine;
    let src = SourceFile {
        path: PathBuf::from("q.sql"),
        language: Language::Sql,
        content: "SELECT a FROM t\n".into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Sql, "sqruff", Kind::Format),
    );
    assert_eq!(default, "SELECT a FROM t\n", "keywords stay upper-case by default");

    for id in ["\"capitalisation.keywords\"", "CP01"] {
        let cfg = engine_cfg_from(
            &format!("[fmt.sql.sqruff.rules.{id}]\ncapitalisation_policy = \"lower\"\n"),
            &Language::Sql,
            "sqruff",
            Kind::Format,
        );
        assert_eq!(
            formatted(&engine, &src, &cfg),
            "select a from t\n",
            "[rules.{id}] capitalisation_policy must reach sqruff"
        );
    }
}

/// A rule id that would break out of its INI section is dropped rather than
/// interpolated: `FluffConfig::from_source` panics on a malformed document.
#[test]
fn sqruff_rejects_a_rule_id_that_would_forge_an_ini_section() {
    let engine = SqruffEngine;
    let src = SourceFile {
        path: PathBuf::from("q.sql"),
        language: Language::Sql,
        content: "SELECT a FROM t\n".into(),
    };
    let cfg = engine_cfg_from(
        "[fmt.sql.sqruff.rules.\"x]\\n[sqruff]\\nmax_line_length\"]\ncapitalisation_policy = \"lower\"\n",
        &Language::Sql,
        "sqruff",
        Kind::Format,
    );
    assert_eq!(
        formatted(&engine, &src, &cfg),
        "SELECT a FROM t\n",
        "a structurally unsafe rule id must be skipped, not emitted into the INI"
    );
}

/// `[lint.<lang>.typos]` accepts the same keys as the language-agnostic
/// `[lint.typos]` table. Only `extend_ignore_words` used to be read from the
/// per-language table; the other six keys were dropped before the engine.
#[test]
fn typos_honors_per_language_keys_beyond_extend_ignore_words() {
    let engine = TyposEngine;

    const KNOWN_BAD: &str = include_str!("fixtures/typos/known_bad.txt");
    let src = SourceFile {
        path: PathBuf::from("doc.txt"),
        language: Language::Markdown,
        content: KNOWN_BAD.into(),
    };
    assert!(
        !engine.lint(&src, &typos_default_cfg()).unwrap().is_empty(),
        "known_bad.txt must produce diagnostics with default config"
    );

    let cfg = engine_cfg_from(
        "[lint.markdown.typos]\nextend_exclude = [\"doc.txt\"]\n",
        &Language::Markdown,
        "typos",
        Kind::Lint,
    );
    assert!(
        engine.lint(&src, &cfg).unwrap().is_empty(),
        "per-language extend_exclude must skip the file, exactly as the global table does"
    );
}

/// A selector list wide enough that the print width decides whether it wraps.
const WIDE_CSS: &str = ".aaaaaaaaaa, .bbbbbbbbbb, .cccccccccc, .dddddddddd, .eeeeeeeeee, .ffffffffff { color: red; }\n";

/// The opinionated `[defaults] line_length` is a layer *under* the user's table
/// (ADR 0006/0007), not over it. `print_width` deserialized into the upstream
/// options struct and was then unconditionally clobbered by the global.
#[test]
fn malva_user_print_width_overrides_the_global() {
    let engine = poly_core::engines::malva::MalvaEngine;
    let src = SourceFile {
        path: PathBuf::from("a.css"),
        language: Language::Css,
        content: WIDE_CSS.into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Css, "malva", Kind::Format),
    );
    let user = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.css.malva]\nprint_width = 30\n",
            &Language::Css,
            "malva",
            Kind::Format,
        ),
    );
    assert_ne!(user, default, "a user-set print_width must reach malva");

    // The global still applies when the user leaves the key unset.
    let mut narrowed_global = engine_cfg_from("", &Language::Css, "malva", Kind::Format);
    narrowed_global.globals.line_length = 30;
    assert_eq!(
        formatted(&engine, &src, &narrowed_global),
        user,
        "with print_width unset, [defaults] line_length still drives the width"
    );
}

#[test]
fn graphql_user_print_width_overrides_the_global() {
    let engine = GraphQlEngine;
    let src = SourceFile {
        path: PathBuf::from("q.graphql"),
        language: Language::GraphQl,
        content: "{ field(argumentOne: 1, argumentTwo: 2, argumentThree: 3, argumentFour: 4) }\n".into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::GraphQl, "graphql", Kind::Format),
    );
    let user = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.graphql.graphql]\nprint_width = 20\n",
            &Language::GraphQl,
            "graphql",
            Kind::Format,
        ),
    );
    assert_ne!(user, default, "a user-set print_width must reach pretty_graphql");
}

#[test]
fn yaml_user_print_width_overrides_the_global() {
    let engine = poly_core::engines::yaml::YamlEngine;
    let src = SourceFile {
        path: PathBuf::from("a.yaml"),
        language: Language::Yaml,
        content: "a: [1111111, 2222222, 3333333, 4444444, 5555555, 6666666, 7777777, 8888888]\n".into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Yaml, "yaml", Kind::Format),
    );
    let user = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.yaml.yaml]\nprint_width = 20\n",
            &Language::Yaml,
            "yaml",
            Kind::Format,
        ),
    );
    assert_ne!(user, default, "a user-set print_width must reach pretty_yaml");
}

#[test]
fn markup_fmt_user_print_width_overrides_the_global() {
    let engine = poly_core::engines::markup_fmt::MarkupFmtEngine;
    let src = SourceFile {
        path: PathBuf::from("a.html"),
        language: Language::Html,
        content: "<div><span>aaaaaaa</span><span>bbbbbbb</span><span>ccccccc</span><span>ddddddd</span></div>\n".into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from("", &Language::Html, "markup_fmt", Kind::Format),
    );
    let user = formatted(
        &engine,
        &src,
        &engine_cfg_from(
            "[fmt.html.markup_fmt]\nprint_width = 20\n",
            &Language::Html,
            "markup_fmt",
            Kind::Format,
        ),
    );
    assert_ne!(user, default, "a user-set print_width must reach markup_fmt");
}
