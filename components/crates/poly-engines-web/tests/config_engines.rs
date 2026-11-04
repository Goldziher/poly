//! Config-contract regressions for the web engines: a key a user writes in `poly.toml` reaches the
//! backend. Grouped apart from the per-backend fixture files because the property under test is the
//! config contract, not the backend's formatting or rule behaviour.

use std::fs;
use std::path::PathBuf;

use poly_core::config::{Config, Kind};
use poly_engine_api::config::EngineConfig;
use poly_engine_api::engine::{Engine, FormatOutput, SourceFile};
use poly_engine_api::language::Language;
use poly_engines_web::{MalvaEngine, MarkupFmtEngine, RumdlEngine, YamlEngine};

/// Load `contents` as a `poly.toml` and slice out one engine's config.
fn engine_cfg_from(contents: &str, language: &Language, engine: &str, kind: Kind) -> EngineConfig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("poly.toml");
    fs::write(&path, contents).unwrap();
    Config::load_file(&path).unwrap().engine_config(language, engine, kind)
}

fn formatted(engine: &dyn Engine, src: &SourceFile, cfg: &EngineConfig) -> String {
    match engine.format(src, cfg).unwrap() {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => src.content.to_string(),
    }
}

/// ADR 0016 promises that any non-`level` key under `[rules.<id>]` is passed to
/// the backend as a tool parameter. rumdl's MD044 (proper names) takes a `names`
/// list, and nothing fires without it — so a diagnostic appearing at all proves
/// the parameter was forwarded.
#[test]
fn rumdl_honors_canonical_per_rule_params() {
    let engine = RumdlEngine;
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

/// A selector list wide enough that the print width decides whether it wraps.
/// A malva-only key that changes no layout: it keeps the table on malva, not the oxfmt-compatible
/// default formatter, so these tests exercise malva's own layering.
const MALVA_ONLY: &str = "[fmt.css.malva]\nuse_tabs = false\n";

const WIDE_CSS: &str = ".aaaaaaaaaa, .bbbbbbbbbb, .cccccccccc, .dddddddddd, .eeeeeeeeee, .ffffffffff { color: red; }\n";

/// The opinionated `[defaults] line_length` is a layer *under* the user's table
/// (ADR 0006/0007), not over it. `print_width` deserialized into the upstream
/// options struct and was then unconditionally clobbered by the global.
#[test]
fn malva_user_print_width_overrides_the_global() {
    let engine = MalvaEngine;
    let src = SourceFile {
        path: PathBuf::from("a.css"),
        language: Language::Css,
        content: WIDE_CSS.into(),
    };

    let default = formatted(
        &engine,
        &src,
        &engine_cfg_from(MALVA_ONLY, &Language::Css, "malva", Kind::Format),
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
    let mut narrowed_global = engine_cfg_from(MALVA_ONLY, &Language::Css, "malva", Kind::Format);
    narrowed_global.globals.line_length = 30;
    assert_eq!(
        formatted(&engine, &src, &narrowed_global),
        user,
        "with print_width unset, [defaults] line_length still drives the width"
    );
}

#[test]
fn yaml_user_print_width_overrides_the_global() {
    let engine = YamlEngine;
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
            "[fmt.yaml.yaml]\nline_width = 20\n",
            &Language::Yaml,
            "yaml",
            Kind::Format,
        ),
    );
    assert_ne!(user, default, "a user-set line_width must reach the yaml formatter");
}

#[test]
fn markup_fmt_user_print_width_overrides_the_global() {
    let engine = MarkupFmtEngine;
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
