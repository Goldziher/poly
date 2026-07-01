//! insta snapshot tests for the rumdl Markdown backend.
//!
//! Two fixtures are required per the project's tdd-and-prek contract:
//! - `bad.md`         — known-bad file asserting expected `Diagnostic`s
//! - `unformatted.md` — known-unformatted file asserting exact formatted output

use std::fs;
use std::path::PathBuf;

use polylint_core::SourceFile;
use polylint_core::config::{EngineConfig, GlobalDefaults};
use polylint_core::engine::{Engine, FormatOutput};
use polylint_core::engines::rumdl::RumdlEngine;
use polylint_core::language::Language;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rumdl")
}

fn default_cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

fn md_src(content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from("test.md"),
        language: Language::Markdown,
        content: content.into(),
    }
}

/// Build an `EngineConfig` whose options table holds a single string-array key.
fn cfg_with_codes(key: &str, codes: &[&str]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert(
        key.to_string(),
        toml::Value::Array(
            codes
                .iter()
                .map(|c| toml::Value::String((*c).into()))
                .collect(),
        ),
    );
    EngineConfig {
        options,
        ..default_cfg()
    }
}

/// Sorted, de-duplicated rule codes present in a diagnostic set.
fn sorted_codes(diags: &[polylint_core::engine::Diagnostic]) -> Vec<String> {
    let mut codes: Vec<String> = diags.iter().filter_map(|d| d.code.clone()).collect();
    codes.sort();
    codes.dedup();
    codes
}

fn load_fixture(name: &str) -> SourceFile {
    let path = fixtures_dir().join(name);
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
    SourceFile {
        path,
        language: Language::Markdown,
        content: content.into(),
    }
}

// ── known-bad: assert expected Diagnostic codes ──────────────────────────────

#[test]
fn bad_md_diagnostics() {
    let engine = RumdlEngine;
    let src = load_fixture("bad.md");
    let cfg = default_cfg();
    let mut diags = engine.lint(&src, &cfg).expect("lint succeeded");
    // Sort for snapshot stability.
    diags.sort_by_key(|d| {
        (
            d.span.as_ref().map(|s| s.start_line).unwrap_or(0),
            d.code.clone(),
        )
    });
    let summary: Vec<_> = diags
        .iter()
        .map(|d| {
            format!(
                "line={} code={} msg={}",
                d.span.as_ref().map(|s| s.start_line).unwrap_or(0),
                d.code.as_deref().unwrap_or("<none>"),
                d.title
            )
        })
        .collect();
    insta::assert_debug_snapshot!("bad_md_diagnostics", summary);
}

// ── canonical rule-selection vocabulary (ADR 0016) ───────────────────────────
//
// rumdl must accept the canonical `select` / `extend_select` / `ignore` keys in
// addition to its native `enable` / `disable` aliases.

#[test]
fn canonical_ignore_matches_native_disable() {
    let engine = RumdlEngine;
    // A wrappable long line (words separated by spaces) so MD013 flags it —
    // a single unbreakable token is exempt from the line-length rule.
    let long_line = vec!["word"; 30].join(" ");
    let content = format!("# Title\n\n{long_line}\n");
    let src = md_src(&content);

    // Baseline: MD013 (line-length) fires on the 130-char line.
    let base = engine.lint(&src, &default_cfg()).unwrap();
    assert!(
        base.iter().any(|d| d.code.as_deref() == Some("MD013")),
        "MD013 must fire on a 130-char line; got: {base:?}"
    );

    let native = engine
        .lint(&src, &cfg_with_codes("disable", &["MD013"]))
        .unwrap();
    let canonical = engine
        .lint(&src, &cfg_with_codes("ignore", &["MD013"]))
        .unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `ignore` must behave like native `disable`"
    );
    assert!(
        !sorted_codes(&native).contains(&"MD013".to_string()),
        "disabling MD013 must suppress it; got: {native:?}"
    );
}

#[test]
fn canonical_select_and_extend_select_match_native_enable() {
    let engine = RumdlEngine;
    // `#Title` trips MD018 (no space after the hash) and the trailing spaces
    // trip MD009 — so an `enable` allow-list of just MD018 must narrow the set
    // to MD018 alone, distinguishing it from the default (multi-rule) run.
    let src = md_src("#Title\n\nsome text with trailing spaces   \n");

    let native = engine
        .lint(&src, &cfg_with_codes("enable", &["MD018"]))
        .unwrap();
    let via_select = engine
        .lint(&src, &cfg_with_codes("select", &["MD018"]))
        .unwrap();
    let via_extend = engine
        .lint(&src, &cfg_with_codes("extend_select", &["MD018"]))
        .unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&via_select),
        "canonical `select` must behave like native `enable`"
    );
    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&via_extend),
        "canonical `extend_select` must behave like native `enable`"
    );
    assert_eq!(
        sorted_codes(&native),
        vec!["MD018".to_string()],
        "an `enable` allow-list of MD018 must narrow the findings to MD018 only; got: {native:?}"
    );
}

// ── known-unformatted: assert exact formatted output ─────────────────────────

#[test]
fn unformatted_md_formats_cleanly() {
    let engine = RumdlEngine;
    let src = load_fixture("unformatted.md");
    let cfg = default_cfg();
    let output = engine.format(&src, &cfg).expect("format succeeded");
    match output {
        FormatOutput::Formatted(formatted) => {
            insta::assert_snapshot!("unformatted_md_formatted", formatted);
        }
        FormatOutput::Unchanged => panic!(
            "expected Formatted for unformatted.md but got Unchanged — \
             check that the fixture still has trailing whitespace"
        ),
    }
}
