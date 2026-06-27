//! Insta snapshot fixtures for the markup_fmt HTML / Vue / Svelte backend.
//!
//! markup_fmt is format-only. Fixtures:
//! - `markup_fmt_known_unformatted_html` — messy HTML → canonical output.
//! - `markup_fmt_known_unformatted_vue` — a Vue SFC template → canonical output.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::markup_fmt::MarkupFmtEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language,
        content: content.into(),
    }
}

fn format_to_string(path: &str, language: Language, content: &str) -> String {
    let engine = MarkupFmtEngine;
    match engine
        .format(&make_src(path, language, content), &engine_cfg())
        .unwrap()
    {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => content.to_string(),
    }
}

const KNOWN_UNFORMATTED_HTML: &str = "\
<html><head><title>Hi</title></head><body><p class=\"a\"   id=\"b\">Hello   world</p><div><span>x</span></div></body></html>";

#[test]
fn markup_fmt_known_unformatted_html() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_html",
        format_to_string("page.html", Language::Html, KNOWN_UNFORMATTED_HTML)
    );
}

const KNOWN_UNFORMATTED_VUE: &str = "\
<template><div class=\"box\"><Button   :label=\"title\" @click=\"onClick\">Go</Button></div></template>";

#[test]
fn markup_fmt_known_unformatted_vue() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_vue",
        format_to_string("App.vue", Language::Vue, KNOWN_UNFORMATTED_VUE)
    );
}
