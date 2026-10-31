//! insta snapshot tests for the adrlint MADR backend.
//!
//! adrlint is a linter only — its formatter is rumdl, which poly already runs — so the
//! backend's contract is one **known-bad** fixture asserting the expected diagnostics plus
//! a **known-good** one proving a conforming record is clean.

use std::fs;
use std::path::PathBuf;

use poly_core::SourceFile;
use poly_core::config::{EngineConfig, GlobalDefaults};
use poly_core::engine::{Engine, Severity};
use poly_core::engines::adrlint::AdrlintEngine;
use poly_core::language::Language;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/adrlint")
}

fn cfg(enabled: bool) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert("enabled".to_string(), toml::Value::Boolean(enabled));
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options,
    }
}

fn load_fixture(name: &str) -> SourceFile {
    let path = fixtures_dir().join(name);
    let content = fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
    SourceFile {
        path,
        language: Language::Markdown,
        content: content.into(),
    }
}

#[test]
fn bad_adr_diagnostics() {
    let mut diags = AdrlintEngine
        .lint(&load_fixture("0001-bad.md"), &cfg(true))
        .expect("lint succeeded");
    diags.sort_by_key(|d| (d.span.as_ref().map(|s| s.start_line).unwrap_or(0), d.code.clone()));
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            format!(
                "line={} code={} severity={:?} msg={}",
                d.span.as_ref().map(|s| s.start_line).unwrap_or(0),
                d.code.as_deref().unwrap_or("<none>"),
                d.severity,
                d.title
            )
        })
        .collect();
    insta::assert_debug_snapshot!("bad_adr_diagnostics", summary);
}

#[test]
fn well_formed_adr_is_clean() {
    let diags = AdrlintEngine
        .lint(&load_fixture("0001-good.md"), &cfg(true))
        .expect("lint succeeded");
    assert!(diags.is_empty(), "got: {diags:#?}");
}

#[test]
fn disabled_engine_reports_nothing() {
    let diags = AdrlintEngine
        .lint(&load_fixture("0001-bad.md"), &cfg(false))
        .expect("lint succeeded");
    assert!(diags.is_empty());
}

#[test]
fn non_adr_markdown_is_not_claimed() {
    let src = SourceFile {
        path: PathBuf::from("README.md"),
        language: Language::Markdown,
        content: "# Broken\n".into(),
    };
    assert!(AdrlintEngine.lint(&src, &cfg(true)).expect("lint").is_empty());
}

#[test]
fn structural_heading_violation_carries_a_fix() {
    let src = SourceFile {
        path: PathBuf::from("adrs/0001-use-rust.md"),
        language: Language::Markdown,
        content: "\
# Use Rust

## Context and Problem Statement

Text.

## Decision Outcome

Chosen option: Rust, because it is fast.

## Consequences

Good.
"
        .into(),
    };
    let diags = AdrlintEngine.lint(&src, &cfg(true)).expect("lint");
    let fixable = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("MADR07a") || d.code.as_deref() == Some("MADR06"))
        .expect("the misplaced Consequences heading is reported");
    assert_eq!(fixable.fix.len(), 1, "{fixable:#?}");
    assert_eq!(fixable.fix[0].replacement, "#");
    assert!(fixable.severity == Severity::Error || fixable.severity == Severity::Warning);
}
