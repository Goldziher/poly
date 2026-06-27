//! Insta snapshot fixtures for the taplo TOML backend.
//!
//! - `known_bad_lint_snapshot` — a known-bad TOML file asserts the expected
//!   [`Diagnostic`]s (duplicate key).
//! - `known_unformatted_snapshot` — a known-unformatted TOML file asserts the
//!   exact formatted output produced by the taplo formatter.

use polylint_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, FormatOutput, SourceFile},
    engines::taplo::TaploEngine,
};

fn engine_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: path.into(),
        language: Language::Toml,
        content: content.into(),
    }
}

// ---------------------------------------------------------------------------
// Known-bad fixture: expects lint diagnostics
// ---------------------------------------------------------------------------

/// Known-bad TOML: duplicate key → `duplicate-key` diagnostic.
const KNOWN_BAD: &str = "\
# This TOML file is intentionally invalid.
name = \"polylint\"
name = \"duplicate\"
";

#[test]
fn known_bad_lint_snapshot() {
    let engine = TaploEngine::new();
    let src = make_src("known_bad.toml", KNOWN_BAD);
    let diags = engine.lint(&src, &engine_cfg()).unwrap();

    // Collect a stable, snapshot-friendly summary: (code, message, line).
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            (
                d.code.as_deref().unwrap_or(""),
                d.message.as_str(),
                d.span.as_ref().map(|s| s.start_line),
            )
        })
        .collect();

    insta::assert_debug_snapshot!("known_bad_diagnostics", summary);
}

// ---------------------------------------------------------------------------
// Known-unformatted fixture: expects exact formatted output
// ---------------------------------------------------------------------------

/// Known-unformatted TOML: extra whitespace around `=`, un-spaced array —
/// taplo should normalize it to a canonical form.
const KNOWN_UNFORMATTED: &str = "\
[package]
name  =  \"polylint\"
version = \"0.1.0\"
authors = [\"Alice\",\"Bob\",\"Carol\"]

[dependencies]
anyhow = \"1\"
";

#[test]
fn known_unformatted_snapshot() {
    let engine = TaploEngine::new();
    let src = make_src("known_unformatted.toml", KNOWN_UNFORMATTED);
    let result = engine.format(&src, &engine_cfg()).unwrap();

    let formatted = match result {
        FormatOutput::Formatted(s) => s,
        FormatOutput::Unchanged => KNOWN_UNFORMATTED.to_string(),
    };

    insta::assert_snapshot!("known_unformatted_output", formatted);
}
