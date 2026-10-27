//! `[fmt.python.ruff]` formatter options: each test formats one source with the
//! option set and asserts the output changed in the way the option documents.

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::ruff::RuffEngine,
};

fn format(content: &str, options: &[(&str, toml::Value)]) -> String {
    let config = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: options.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
    };
    let src = SourceFile {
        path: "fixture.py".into(),
        language: Language::Python,
        content: content.into(),
    };
    match RuffEngine.format(&src, &config).unwrap() {
        FormatOutput::Formatted(formatted) => formatted,
        FormatOutput::Unchanged => content.to_string(),
    }
}

fn text(value: &str) -> toml::Value {
    toml::Value::String(value.into())
}

#[test]
fn quote_style_single_rewrites_double_quoted_strings() {
    assert_eq!(format("x = \"a\"\n", &[]), "x = \"a\"\n");
    assert_eq!(format("x = \"a\"\n", &[("quote_style", text("single"))]), "x = 'a'\n");
}

#[test]
fn quote_style_preserve_keeps_the_original_quotes() {
    assert_eq!(format("x = 'a'\n", &[("quote_style", text("preserve"))]), "x = 'a'\n");
}

#[test]
fn indent_style_tab_indents_with_tabs() {
    let source = "if x:\n    y = 1\n";
    assert_eq!(format(source, &[]), source);
    assert_eq!(format(source, &[("indent_style", text("tab"))]), "if x:\n\ty = 1\n");
}

#[test]
fn skip_magic_trailing_comma_collapses_a_call_that_fits() {
    let source = "f(\n    a,\n    b,\n)\n";
    assert_eq!(format(source, &[]), source);
    assert_eq!(
        format(source, &[("skip_magic_trailing_comma", toml::Value::Boolean(true))]),
        "f(a, b)\n"
    );
}

#[test]
fn line_ending_cr_lf_converts_and_lf_restores() {
    assert_eq!(
        format("x = 1\ny = 2\n", &[("line_ending", text("cr-lf"))]),
        "x = 1\r\ny = 2\r\n"
    );
    assert_eq!(
        format("x = 1\r\ny = 2\r\n", &[("line_ending", text("lf"))]),
        "x = 1\ny = 2\n"
    );
}

#[test]
fn line_ending_auto_follows_the_file() {
    let crlf = "x = 1\r\ny = 2\r\n";
    assert_eq!(format(crlf, &[("line_ending", text("auto"))]), crlf);
    let lf = "x = 1\ny = 2\n";
    assert_eq!(format(lf, &[("line_ending", text("auto"))]), lf);
}

#[test]
fn an_unrecognised_value_keeps_the_default() {
    assert_eq!(
        format("x = \"a\"\n", &[("quote_style", text("nonsense"))]),
        "x = \"a\"\n"
    );
}
