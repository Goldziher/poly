//! Snapshot tests for the three output renderers (pretty / json / toon) over a
//! synthetic result set that includes a diagnostic with populated `metadata`
//! and one without. Color is forced off so the `pretty` snapshot is stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use poly_core::report::{self, Verbosity};
use poly_core::runner::{EngineDebug, FormatResult, LintResult, RunDebug};
use poly_core::{Diagnostic, Edit, Severity, Span};

fn sample_lint_results() -> Vec<LintResult> {
    let mut metadata = BTreeMap::new();
    metadata.insert("category".to_string(), "style".to_string());
    metadata.insert("url".to_string(), "https://example.test/rules/E501".to_string());

    vec![
        LintResult {
            path: PathBuf::from("src/main.py"),
            diagnostics: vec![
                Diagnostic {
                    engine: "ruff".to_string(),
                    code: Some("E501".to_string()),
                    severity: Severity::Warning,
                    title: "line too long".to_string(),
                    description: Some("the line exceeds the configured width".to_string()),
                    span: Some(Span {
                        start_line: 12,
                        start_col: 80,
                        end_line: 12,
                        end_col: 95,
                    }),
                    url: Some("https://example.test/rules/E501".to_string()),
                    fix: vec![],
                    metadata,
                },
                Diagnostic {
                    engine: "ruff".to_string(),
                    code: None,
                    severity: Severity::Error,
                    title: "syntax error".to_string(),
                    description: None,
                    span: Some(Span {
                        start_line: 1,
                        start_col: 1,
                        end_line: 1,
                        end_col: 1,
                    }),
                    url: None,
                    fix: vec![],
                    metadata: BTreeMap::new(),
                },
            ],
            debug: None,
        },
        LintResult {
            path: PathBuf::from("src/clean.py"),
            diagnostics: vec![],
            debug: None,
        },
    ]
}

fn sample_format_results() -> Vec<FormatResult> {
    vec![
        FormatResult {
            path: PathBuf::from("src/main.py"),
            changed: true,
            formatted: Some("formatted".to_string()),
            debug: None,
        },
        FormatResult {
            path: PathBuf::from("src/clean.py"),
            changed: false,
            formatted: None,
            debug: None,
        },
    ]
}

#[test]
fn lint_pretty_default_is_terse_without_description_url_or_metadata() {
    owo_colors::set_override(false);
    let (text, total) = report::render_lint_pretty(&sample_lint_results(), Verbosity::default());
    assert_eq!(total, 2, "two diagnostics across the result set");
    // Default view hides description, url, and metadata.
    assert!(
        !text.contains("the line exceeds the configured width"),
        "default view must not show description"
    );
    assert!(!text.contains("category=style"), "default view must not show metadata");
    insta::assert_snapshot!("lint_pretty", text);
}

#[test]
fn lint_pretty_verbose_shows_description_url_and_metadata() {
    owo_colors::set_override(false);
    let verbose = Verbosity::new(true, false);
    let (text, total) = report::render_lint_pretty(&sample_lint_results(), verbose);
    assert_eq!(total, 2);
    assert!(
        text.contains("the line exceeds the configured width"),
        "--verbose must show description"
    );
    assert!(
        text.contains("https://example.test/rules/E501"),
        "--verbose must show url"
    );
    assert!(text.contains("category=style"), "--verbose must show metadata");
    insta::assert_snapshot!("lint_pretty_verbose", text);
}

#[test]
fn lint_pretty_reports_autofixable_count() {
    owo_colors::set_override(false);
    // Two findings, only one carrying a suggested `fix` edit: the summary must
    // report the total and, on its own line, how many are fixable with `--fix`.
    let results = vec![LintResult {
        path: PathBuf::from("src/main.py"),
        diagnostics: vec![
            Diagnostic {
                engine: "ruff".to_string(),
                code: Some("F401".to_string()),
                severity: Severity::Warning,
                title: "unused import".to_string(),
                description: None,
                span: Some(Span {
                    start_line: 1,
                    start_col: 1,
                    end_line: 1,
                    end_col: 20,
                }),
                url: None,
                fix: vec![Edit {
                    start_byte: 0,
                    end_byte: 20,
                    replacement: String::new(),
                }],
                metadata: BTreeMap::new(),
            },
            Diagnostic {
                engine: "ruff".to_string(),
                code: None,
                severity: Severity::Error,
                title: "syntax error".to_string(),
                description: None,
                span: None,
                url: None,
                fix: vec![],
                metadata: BTreeMap::new(),
            },
        ],
        debug: None,
    }];

    let (text, total) = report::render_lint_pretty(&results, Verbosity::default());
    assert_eq!(total, 2, "two diagnostics in the result set");
    assert!(text.contains("2 issue(s) found."), "missing total line, got:\n{text}");
    assert!(
        text.contains("1 fixable with the `--fix` option."),
        "missing autofixable count, got:\n{text}"
    );
    insta::assert_snapshot!("lint_pretty_fixable", text);
}

/// Boundary: when *every* finding carries an autofix, the fixable count equals
/// the total and the hint reports all of them.
#[test]
fn lint_pretty_reports_all_findings_fixable_when_every_diagnostic_has_a_fix() {
    owo_colors::set_override(false);
    let fixable_diagnostic = |code: &str| Diagnostic {
        engine: "ruff".to_string(),
        code: Some(code.to_string()),
        severity: Severity::Warning,
        title: "unused import".to_string(),
        description: None,
        span: None,
        url: None,
        fix: vec![Edit {
            start_byte: 0,
            end_byte: 1,
            replacement: String::new(),
        }],
        metadata: BTreeMap::new(),
    };
    let results = vec![LintResult {
        path: PathBuf::from("src/main.py"),
        diagnostics: vec![fixable_diagnostic("F401"), fixable_diagnostic("F811")],
        debug: None,
    }];

    let (text, total) = report::render_lint_pretty(&results, Verbosity::default());
    assert_eq!(total, 2);
    assert!(text.contains("2 issue(s) found."), "got:\n{text}");
    assert!(
        text.contains("2 fixable with the `--fix` option."),
        "every finding is fixable, so the count must equal the total; got:\n{text}"
    );
}

/// When no finding carries an autofix, the summary must not print a fixable
/// hint at all (rather than a misleading "0 fixable").
#[test]
fn lint_pretty_omits_fixable_line_when_nothing_is_fixable() {
    owo_colors::set_override(false);
    let (text, _total) = report::render_lint_pretty(&sample_lint_results(), Verbosity::default());
    assert!(
        !text.contains("fixable"),
        "must not mention fixable when no finding has an autofix, got:\n{text}"
    );
}

#[test]
fn lint_json_renders_full_envelope() {
    let json = report::report_lint_json(&sample_lint_results());
    insta::assert_snapshot!("lint_json", json);
}

#[test]
fn lint_toon_renders_full_envelope() {
    let toon = report::report_lint_toon(&sample_lint_results());
    insta::assert_snapshot!("lint_toon", toon);
}

#[test]
fn format_pretty_lists_changed_files() {
    owo_colors::set_override(false);
    let (text, changed) = report::render_format_pretty(&sample_format_results(), false, Verbosity::default());
    assert_eq!(changed, 1, "one file changed");
    insta::assert_snapshot!("format_pretty", text);
}

#[test]
fn format_pretty_dry_run_uses_future_tense() {
    owo_colors::set_override(false);
    // `check = true` is the dry-run (no `--fix`): the summary must say the files
    // *will* change, not that they were changed.
    let (text, changed) = report::render_format_pretty(&sample_format_results(), true, Verbosity::default());
    assert_eq!(changed, 1, "one file would change");
    assert!(
        text.contains("will change"),
        "dry-run summary must use future tense, got: {text}"
    );
    assert!(
        !text.contains("1 changed of"),
        "dry-run summary must not use the past-tense '\u{2026} changed of \u{2026}' wording, got: {text}"
    );
    insta::assert_snapshot!("format_pretty_dry_run", text);
}

#[test]
fn format_json_lists_results() {
    let json = report::report_format_json(&sample_format_results());
    insta::assert_snapshot!("format_json", json);
}

#[test]
fn format_toon_lists_results() {
    let toon = report::report_format_toon(&sample_format_results());
    insta::assert_snapshot!("format_toon", toon);
}

/// `--debug` pretty output: the dim `[debug] <engine> v<ver>  ran|cache hit
/// <ms>` block must render after the diagnostic lines for each file.
///
/// Two engine entries are used:
///   - `ruff v0.11.0` with `cache_hit = false` → "ran"
///   - `typos v1.32.0` with `cache_hit = true` → "cache hit"
///
/// `duration_ms` values are fixed constants so the snapshot is deterministic.
/// The result also has one diagnostic (to verify diagnostics and the debug
/// block coexist in the output).
#[test]
fn lint_pretty_debug_renders_engine_timing_block() {
    owo_colors::set_override(false);

    let results = vec![LintResult {
        path: PathBuf::from("src/main.py"),
        diagnostics: vec![Diagnostic {
            engine: "ruff".to_string(),
            code: Some("E501".to_string()),
            severity: Severity::Warning,
            title: "line too long".to_string(),
            description: None,
            span: Some(Span {
                start_line: 12,
                start_col: 80,
                end_line: 12,
                end_col: 95,
            }),
            url: None,
            fix: vec![],
            metadata: BTreeMap::new(),
        }],
        debug: Some(RunDebug {
            engines: vec![
                EngineDebug {
                    engine: "ruff".to_string(),
                    version: "0.11.0".to_string(),
                    duration_ms: 1.00_f64,
                    cache_hit: false,
                },
                EngineDebug {
                    engine: "typos".to_string(),
                    version: "1.32.0".to_string(),
                    duration_ms: 0.00_f64,
                    cache_hit: true,
                },
            ],
        }),
    }];

    let (text, total) = report::render_lint_pretty(&results, Verbosity::new(false, true));

    assert_eq!(total, 1, "one diagnostic in the result set");

    // The debug block must appear (path header + diagnostic + debug entries).
    assert!(
        text.contains("[debug] ruff"),
        "--debug must render the ruff engine block; got:\n{text}"
    );
    assert!(
        text.contains("[debug] typos"),
        "--debug must render the typos engine block; got:\n{text}"
    );
    // cache_hit=false → "ran"
    assert!(
        text.contains("ran"),
        "--debug must render 'ran' for cache_hit=false; got:\n{text}"
    );
    // cache_hit=true → "cache hit"
    assert!(
        text.contains("cache hit"),
        "--debug must render 'cache hit' for cache_hit=true; got:\n{text}"
    );

    insta::assert_snapshot!("lint_pretty_debug", text);
}
