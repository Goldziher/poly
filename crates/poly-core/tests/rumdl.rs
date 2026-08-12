//! insta snapshot tests for the rumdl Markdown backend.
//!
//! Two fixtures are required per the project's tdd-and-prek contract:
//! - `bad.md`         — known-bad file asserting expected `Diagnostic`s
//! - `unformatted.md` — known-unformatted file asserting exact formatted output

use std::fs;
use std::path::PathBuf;

use poly_core::SourceFile;
use poly_core::config::{EngineConfig, GlobalDefaults};
use poly_core::engine::{Engine, FormatOutput};
use poly_core::engines::rumdl::RumdlEngine;
use poly_core::language::Language;

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
        toml::Value::Array(codes.iter().map(|c| toml::Value::String((*c).into())).collect()),
    );
    EngineConfig {
        options,
        ..default_cfg()
    }
}

/// Sorted, de-duplicated rule codes present in a diagnostic set.
fn sorted_codes(diags: &[poly_core::engine::Diagnostic]) -> Vec<String> {
    let mut codes: Vec<String> = diags.iter().filter_map(|d| d.code.clone()).collect();
    codes.sort();
    codes.dedup();
    codes
}

fn load_fixture(name: &str) -> SourceFile {
    load_fixture_as(name, Language::Markdown)
}

fn load_fixture_as(name: &str, language: Language) -> SourceFile {
    let path = fixtures_dir().join(name);
    let content = fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"));
    SourceFile {
        path,
        language,
        content: content.into(),
    }
}

#[test]
fn bad_md_diagnostics() {
    let engine = RumdlEngine;
    let src = load_fixture("bad.md");
    let cfg = default_cfg();
    let mut diags = engine.lint(&src, &cfg).expect("lint succeeded");
    diags.sort_by_key(|d| (d.span.as_ref().map(|s| s.start_line).unwrap_or(0), d.code.clone()));
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

#[test]
fn canonical_ignore_matches_native_disable() {
    let engine = RumdlEngine;
    let src = md_src("#Title\n\nContent.\n");

    let base = engine.lint(&src, &default_cfg()).unwrap();
    assert!(
        base.iter().any(|d| d.code.as_deref() == Some("MD018")),
        "MD018 must fire on a heading with no space after '#'; got: {base:?}"
    );

    let native = engine.lint(&src, &cfg_with_codes("disable", &["MD018"])).unwrap();
    let canonical = engine.lint(&src, &cfg_with_codes("ignore", &["MD018"])).unwrap();

    assert_eq!(
        sorted_codes(&native),
        sorted_codes(&canonical),
        "canonical `ignore` must behave like native `disable`"
    );
    assert!(
        !sorted_codes(&native).contains(&"MD018".to_string()),
        "disabling MD018 must suppress it; got: {native:?}"
    );
}

#[test]
fn canonical_select_and_extend_select_match_native_enable() {
    let engine = RumdlEngine;
    let src = md_src("#Title\n\nsome text with trailing spaces   \n");

    let native = engine.lint(&src, &cfg_with_codes("enable", &["MD018"])).unwrap();
    let via_select = engine.lint(&src, &cfg_with_codes("select", &["MD018"])).unwrap();
    let via_extend = engine.lint(&src, &cfg_with_codes("extend_select", &["MD018"])).unwrap();

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

/// A document whose headings end in a literal `#` (`C#`, `F#`). Per CommonMark
/// §4.2 a closing sequence of `#`s must be preceded by whitespace, so these are
/// ordinary open ATX headings whose *text* ends in `#` — not malformed closed
/// headings. rumdl's MD020 misreads them; poly guards the rule (see
/// `engines::rumdl`), and this fixture is the regression.
const HEADINGS_ENDING_IN_HASH: &str = "# Languages\n\n## Overview\n\n### C#\n\nText.\n\n### F#\n\nMore text.\n";

#[test]
fn heading_text_ending_in_hash_survives_format_byte_identically() {
    let engine = RumdlEngine;
    let src = md_src(HEADINGS_ENDING_IN_HASH);
    match engine.format(&src, &default_cfg()).expect("format succeeded") {
        FormatOutput::Unchanged => {}
        FormatOutput::Formatted(out) => {
            panic!("`### C#` must survive formatting byte-identically, but the content was rewritten:\n{out}")
        }
    }
}

#[test]
fn heading_text_ending_in_hash_reports_no_md020() {
    let engine = RumdlEngine;
    let src = md_src(HEADINGS_ENDING_IN_HASH);
    let diags = engine.lint(&src, &default_cfg()).expect("lint succeeded");
    assert!(
        !sorted_codes(&diags).contains(&"MD020".to_string()),
        "`### C#` is a valid open ATX heading, MD020 must not fire; got: {diags:?}"
    );
}

#[test]
fn closed_atx_heading_with_missing_spaces_is_still_fixed() {
    // `##Foo##` really is a closed ATX heading with both spaces missing — the
    // guard must not disable MD020 wholesale.
    let engine = RumdlEngine;
    let src = md_src("##Foo##\n");
    match engine.format(&src, &default_cfg()).expect("format succeeded") {
        FormatOutput::Formatted(out) => assert_eq!(out, "## Foo ##\n", "MD020 must still fix a genuine `##Foo##`"),
        FormatOutput::Unchanged => panic!("expected MD020 to fix `##Foo##`, got Unchanged"),
    }
}

#[test]
fn well_formed_closed_atx_headings_are_unchanged() {
    let engine = RumdlEngine;
    let src = md_src("# Foo #\n\nText.\n\n## Bar ##\n\nMore text.\n");
    assert!(
        matches!(
            engine.format(&src, &default_cfg()).expect("format succeeded"),
            FormatOutput::Unchanged
        ),
        "well-formed closed ATX headings must be left alone"
    );
}

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

#[test]
fn unformatted_mdx_formats_cleanly() {
    let engine = RumdlEngine;
    let src = load_fixture_as("unformatted.mdx", Language::Mdx);
    let cfg = default_cfg();
    let output = engine.format(&src, &cfg).expect("format succeeded");
    match output {
        FormatOutput::Formatted(formatted) => {
            insta::assert_snapshot!("unformatted_mdx_formatted", formatted);
        }
        FormatOutput::Unchanged => panic!(
            "expected Formatted for unformatted.mdx but got Unchanged — \
             check that the fixture still has trailing whitespace"
        ),
    }
}
