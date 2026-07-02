//! Insta snapshot fixtures for the markup_fmt backend.
//!
//! markup_fmt is format-only (no diagnostics). One known-unformatted fixture per
//! language family asserts the exact canonical output:
//!
//! - `markup_fmt_known_unformatted_html`     — plain HTML
//! - `markup_fmt_known_unformatted_vue`      — Vue SFC template
//! - `markup_fmt_known_unformatted_svelte`   — Svelte component
//! - `markup_fmt_known_unformatted_astro`    — Astro component
//! - `markup_fmt_known_unformatted_angular`  — Angular component template (`*.component.html`)
//! - `markup_fmt_known_unformatted_jinja`    — Jinja2 / Twig / Nunjucks template
//! - `markup_fmt_known_unformatted_vento`    — Vento template
//! - `markup_fmt_known_unformatted_mustache` — Mustache / Handlebars template
//! - `markup_fmt_known_unformatted_xml`      — XML document

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
    let label = format!("{language:?}");
    let engine = MarkupFmtEngine;
    match engine
        .format(&make_src(path, language, content), &engine_cfg())
        .unwrap()
    {
        FormatOutput::Formatted(text) => text,
        // Every fixture below is genuinely unformatted, so a no-op would mean the
        // fixture (or the routing) regressed — fail loudly rather than snapshot the
        // input verbatim (which would make the assertion vacuous).
        FormatOutput::Unchanged => {
            panic!("fixture for {path} ({label}) was returned unchanged — it must be unformatted")
        }
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

// --- Svelte -------------------------------------------------------------------

const KNOWN_UNFORMATTED_SVELTE: &str = "\
<script>let name = \"world\";</script><main><h1   class=\"title\">Hello {name}</h1><button on:click={go}>Go</button></main>";

#[test]
fn markup_fmt_known_unformatted_svelte() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_svelte",
        format_to_string("App.svelte", Language::Svelte, KNOWN_UNFORMATTED_SVELTE)
    );
}

// --- Astro --------------------------------------------------------------------

const KNOWN_UNFORMATTED_ASTRO: &str = "\
---
const title = \"Hello\";
---
<html><head><title>{title}</title></head><body><p class=\"text\">Welcome</p></body></html>";

#[test]
fn markup_fmt_known_unformatted_astro() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_astro",
        format_to_string("index.astro", Language::Astro, KNOWN_UNFORMATTED_ASTRO)
    );
}

// --- Angular ------------------------------------------------------------------
// Angular component templates use the `*.component.html` filename convention.

const KNOWN_UNFORMATTED_ANGULAR: &str = "\
<div class=\"container\"><p [class]=\"active ? 'on' : 'off'\">Hello</p><button (click)=\"doIt()\">Click</button></div>";

#[test]
fn markup_fmt_known_unformatted_angular() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_angular",
        format_to_string("app.component.html", Language::Angular, KNOWN_UNFORMATTED_ANGULAR,)
    );
}

// --- Jinja (covers Twig / Nunjucks) -------------------------------------------

// Use simple {{ variable }} interpolation — markup_fmt handles these inline and
// still reformats the surrounding HTML structure.
const KNOWN_UNFORMATTED_JINJA: &str = "\
<html><head><title>{{ page_title }}</title></head><body><h1>{{ heading }}</h1><p class=\"intro\">{{ body }}</p></body></html>";

#[test]
fn markup_fmt_known_unformatted_jinja() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_jinja",
        format_to_string("base.jinja2", Language::Jinja, KNOWN_UNFORMATTED_JINJA)
    );
}

// --- Vento --------------------------------------------------------------------

const KNOWN_UNFORMATTED_VENTO: &str = "\
<html><body><p>{{ title }}</p><ul>{{ for item of items }}<li>{{ item }}</li>{{ /for }}</ul></body></html>";

#[test]
fn markup_fmt_known_unformatted_vento() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_vento",
        format_to_string("layout.vto", Language::Vento, KNOWN_UNFORMATTED_VENTO)
    );
}

// --- Mustache (covers Handlebars) ---------------------------------------------

// Use simple {{ variable }} interpolation to avoid block-tag conservatism.
const KNOWN_UNFORMATTED_MUSTACHE: &str = "\
<html><head><title>{{title}}</title></head><body><p class=\"greeting\">{{message}}</p></body></html>";

#[test]
fn markup_fmt_known_unformatted_mustache() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_mustache",
        format_to_string("list.mustache", Language::Mustache, KNOWN_UNFORMATTED_MUSTACHE)
    );
}

// --- XML ----------------------------------------------------------------------

const KNOWN_UNFORMATTED_XML: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?><root><child attr=\"value\">text content</child><empty/></root>";

#[test]
fn markup_fmt_known_unformatted_xml() {
    insta::assert_snapshot!(
        "markup_fmt_known_unformatted_xml",
        format_to_string("config.xml", Language::Xml, KNOWN_UNFORMATTED_XML)
    );
}

// --- LanguageOptions wiring ---------------------------------------------------

/// A markup_fmt LanguageOptions field set via `[fmt.html.markup_fmt]` reaches
/// the formatter: `quotes = "single"` switches attribute quotes to single.
#[test]
fn markup_fmt_honors_language_option() {
    let engine = MarkupFmtEngine;
    let src = make_src("q.html", Language::Html, "<a href=\"x\">y</a>\n");
    let mut options = toml::Table::new();
    options.insert("quotes".to_string(), toml::Value::String("single".into()));
    let cfg = EngineConfig {
        options,
        ..engine_cfg()
    };
    let FormatOutput::Formatted(out) = engine.format(&src, &cfg).unwrap() else {
        panic!("`quotes = single` should switch attribute quotes");
    };
    assert!(
        out.contains("href='x'"),
        "[fmt.html.markup_fmt] quotes must reach markup_fmt; got: {out}"
    );
}
