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

/// An `h1` followed directly by an `h3` — MD001 (heading increment). The
/// violation is real, but the repair is ambiguous: the author may have meant an
/// `h2` here, or may have meant to add a missing `h2` above. rumdl picks one and
/// demotes the heading, which rewrites the document outline.
const SKIPPED_HEADING_LEVEL: &str = "# Test\n\n### Go\n\ntext\n";

#[test]
fn format_does_not_restructure_heading_levels() {
    // `poly fmt` is a formatter: it may not change a document's outline. MD001
    // is reported by `lint` instead, where the author decides the repair.
    let engine = RumdlEngine;
    let src = md_src(SKIPPED_HEADING_LEVEL);
    match engine.format(&src, &default_cfg()).expect("format succeeded") {
        FormatOutput::Unchanged => {}
        FormatOutput::Formatted(out) => {
            panic!("fmt must not demote `### Go` to `## Go`; the outline was rewritten:\n{out}")
        }
    }
}

#[test]
fn lint_still_reports_skipped_heading_level() {
    // The guard must not silence the diagnostic — only decline to guess the fix.
    let engine = RumdlEngine;
    let src = md_src(SKIPPED_HEADING_LEVEL);
    let diags = engine.lint(&src, &default_cfg()).expect("lint succeeded");
    assert!(
        sorted_codes(&diags).contains(&"MD001".to_string()),
        "MD001 must still be reported to the author; got: {diags:?}"
    );
}

#[test]
fn lint_offers_no_autofix_for_a_skipped_heading_level() {
    // `poly lint --fix` applies diagnostic edits, so carrying one here would
    // restructure the outline by the other path — the guard has to cover both.
    let engine = RumdlEngine;
    let src = md_src(SKIPPED_HEADING_LEVEL);
    let diags = engine.lint(&src, &default_cfg()).expect("lint succeeded");
    let md001 = diags
        .iter()
        .find(|d| d.code.as_deref() == Some("MD001"))
        .expect("MD001 is reported");
    assert!(
        md001.fix.is_empty(),
        "MD001 must be report-only: demoting the heading and inserting the missing level are both \
         valid repairs, so the choice is the author's; got: {:?}",
        md001.fix
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

// --- Go/Helm template detection is code-block aware (Markdown + MDX) ---------
//
// Markdown routinely *documents* template syntax inside code. poly's own
// CHANGELOG does, and used to be skipped wholesale because of it. Template
// actions inside a fenced block, an indented block, or an inline code span are
// documentation; only actions in live prose mark the file as a real template.

/// The exact reason string the backends report for a Go/Helm template file.
const TEMPLATE_SKIP: &str = "Go/Helm template syntax";

/// A CHANGELOG-shaped document: every template action sits inside code.
const CHANGELOG_SHAPED: &str = "\
# Changelog

## 0.1.0

- `poly fmt` no longer reports a file it declined to inspect as checked:

  ```console
  $ poly fmt --check Taskfile.yaml     # skipped: contains {{.CLI_ARGS}}
  All formatted. (1 file(s) scanned)
  ```

- Templates are detected by scanning content for actions (`{{ .Values.x }}`,
  `{{- if … }}`, `{{/* … */}}`), not by filename.
";

#[test]
fn fenced_and_inline_template_syntax_does_not_skip_markdown() {
    let engine = RumdlEngine;
    let src = md_src(CHANGELOG_SHAPED);
    assert_eq!(
        engine.skip_reason(&src),
        None,
        "a changelog that only documents template syntax must be checked, not skipped"
    );
}

#[test]
fn a_documented_template_is_actually_formatted_not_bypassed() {
    let engine = RumdlEngine;
    // Trailing whitespace on the prose line is the observable proof that the
    // formatter ran instead of short-circuiting on the template guard.
    let content = format!("{CHANGELOG_SHAPED}\nTrailing whitespace here.   \n");
    let src = md_src(&content);
    assert_eq!(engine.skip_reason(&src), None);
    let formatted = match engine.format(&src, &default_cfg()).expect("format succeeded") {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => panic!("expected the trailing whitespace to be fixed"),
    };
    assert!(
        formatted.contains("Trailing whitespace here.\n"),
        "trailing whitespace must be stripped; got: {formatted:?}"
    );
    assert!(
        formatted.contains("{{.CLI_ARGS}}"),
        "the documented template action must survive verbatim; got: {formatted:?}"
    );
}

#[test]
fn template_syntax_outside_a_code_block_still_skips_markdown() {
    let engine = RumdlEngine;
    let src = md_src("# {{ .Chart.Name }}\n\nRelease {{ .Release.Name }} is ready.\n");
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
    assert_eq!(engine.lint(&src, &default_cfg()).unwrap().len(), 0);
    assert!(matches!(
        engine.format(&src, &default_cfg()).expect("format succeeded"),
        FormatOutput::Unchanged
    ));
}

#[test]
fn template_syntax_after_a_closed_fence_still_skips_markdown() {
    let engine = RumdlEngine;
    let src = md_src("```\n{{ .Values.a }}\n```\n\nLive: {{ .Values.b }}\n");
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
}

#[test]
fn longer_and_nested_fences_with_info_strings_are_code() {
    let engine = RumdlEngine;
    let src =
        md_src("Docs:\n\n````markdown\n```yaml\nimage: {{ .Values.image }}\n```\n````\n\n~~~yaml\n{{- if .x }}\n~~~\n");
    assert_eq!(
        engine.skip_reason(&src),
        None,
        "a longer outer fence must swallow the inner fence and its info string"
    );
}

#[test]
fn a_short_fence_does_not_close_a_longer_one() {
    let engine = RumdlEngine;
    // The inner ``` must NOT close the ```` block, so the trailing action stays
    // inside code and the file is still formatted.
    let src = md_src("````\n```\n{{ .Values.x }}\n```\n````\n\nPlain prose.\n");
    assert_eq!(engine.skip_reason(&src), None);
}

#[test]
fn an_unterminated_fence_is_scanned_as_prose_and_still_skips() {
    let engine = RumdlEngine;
    // Conservative direction: declining to format costs nothing, reflowing a
    // real template destroys it. An unclosed fence is a malformed document, so
    // its remainder is re-scanned as live prose.
    let src = md_src("Docs:\n\n```yaml\nimage: {{ .Values.image }}\n");
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
}

#[test]
fn indented_code_blocks_are_code() {
    let engine = RumdlEngine;
    let src = md_src("Example:\n\n    image: {{ .Values.image }}\n\nDone.\n");
    assert_eq!(engine.skip_reason(&src), None);
}

#[test]
fn an_indented_line_continuing_a_paragraph_is_prose() {
    let engine = RumdlEngine;
    // CommonMark: an indented code block cannot interrupt a paragraph.
    let src = md_src("A wrapped paragraph\n    image: {{ .Values.image }}\n");
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
}

#[test]
fn an_unterminated_inline_span_is_prose() {
    let engine = RumdlEngine;
    let src = md_src("A stray backtick ` then {{ .Values.image }} live.\n");
    assert_eq!(engine.skip_reason(&src), Some(TEMPLATE_SKIP));
}

#[test]
fn mdx_gets_the_same_code_block_carve_out() {
    let engine = RumdlEngine;
    let mut src = md_src(CHANGELOG_SHAPED);
    src.language = Language::Mdx;
    src.path = PathBuf::from("test.mdx");
    assert_eq!(engine.skip_reason(&src), None);

    let mut live = md_src("Release {{ .Release.Name }}\n");
    live.language = Language::Mdx;
    live.path = PathBuf::from("live.mdx");
    assert_eq!(engine.skip_reason(&live), Some(TEMPLATE_SKIP));
}

#[test]
fn mdx_object_literals_are_still_not_templates() {
    let engine = RumdlEngine;
    let mut src = md_src("<Note style={{ color: \"red\" }}>hi</Note>\n");
    src.language = Language::Mdx;
    src.path = PathBuf::from("jsx.mdx");
    assert_eq!(engine.skip_reason(&src), None);
}
