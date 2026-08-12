//! Snapshot tests for the three output renderers (pretty / json / toon) over a
//! synthetic result set that includes a diagnostic with populated `metadata`
//! and one without. Color is forced off so the `pretty` snapshot is stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use poly_core::report::{self, Verbosity};
use poly_core::runner::{EngineDebug, FormatResult, FormatRun, LintResult, LintRun, RunDebug};
use poly_core::{Diagnostic, DiscoveryReport, Edit, ExcludedRule, Severity, Span};

fn sample_lint_results() -> Vec<LintResult> {
    let mut metadata = BTreeMap::new();
    metadata.insert("category".to_string(), "style".to_string());
    metadata.insert("url".to_string(), "https://example.test/rules/E501".to_string());

    vec![
        LintResult {
            path: PathBuf::from("src/main.py"),
            fix_withheld_generated: false,
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
            fix_withheld_generated: false,
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
            skipped: None,
            debug: None,
        },
        FormatResult {
            path: PathBuf::from("src/clean.py"),
            changed: false,
            formatted: None,
            skipped: None,
            debug: None,
        },
    ]
}

#[test]
fn lint_pretty_default_is_terse_without_description_url_or_metadata() {
    owo_colors::set_override(false);
    let (text, total) = report::render_lint_pretty(&sample_lint_results(), Verbosity::default());
    assert_eq!(total, 2, "two diagnostics across the result set");
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
    let results = vec![LintResult {
        path: PathBuf::from("src/main.py"),
        fix_withheld_generated: false,
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
        fix_withheld_generated: false,
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
        fix_withheld_generated: false,
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

    assert!(
        text.contains("[debug] ruff"),
        "--debug must render the ruff engine block; got:\n{text}"
    );
    assert!(
        text.contains("[debug] typos"),
        "--debug must render the typos engine block; got:\n{text}"
    );
    assert!(
        text.contains("ran"),
        "--debug must render 'ran' for cache_hit=false; got:\n{text}"
    );
    assert!(
        text.contains("cache hit"),
        "--debug must render 'cache hit' for cache_hit=true; got:\n{text}"
    );

    insta::assert_snapshot!("lint_pretty_debug", text);
}

/// A file every backend declined used to be counted as scanned and reported
/// exactly like one that was checked and found clean, so `All formatted.` could
/// not distinguish "I verified everything" from "I declined to look". The
/// summary now separates the two and names the reason.
#[test]
fn skipped_files_are_reported_separately_from_checked_ones() {
    let results = vec![
        FormatResult {
            path: PathBuf::from("clean.yaml"),
            changed: false,
            formatted: None,
            skipped: None,
            debug: None,
        },
        FormatResult {
            path: PathBuf::from("Taskfile.yaml"),
            changed: false,
            formatted: None,
            skipped: Some("Go/Helm template syntax".to_string()),
            debug: None,
        },
    ];

    let (text, changed) = report::render_format_pretty(&results, true, Verbosity::default());

    assert_eq!(changed, 0);
    assert!(text.contains("1 file(s) checked"), "got: {text}");
    assert!(text.contains("1 skipped (Go/Helm template syntax)"), "got: {text}");
}

/// With several distinct reasons each is counted, so one cause cannot hide
/// behind another.
#[test]
fn distinct_skip_reasons_are_counted_individually() {
    let skip = |path: &str, why: &str| FormatResult {
        path: PathBuf::from(path),
        changed: false,
        formatted: None,
        skipped: Some(why.to_string()),
        debug: None,
    };
    let results = vec![
        skip("a.yaml", "Go/Helm template syntax"),
        skip("b.yaml", "Go/Helm template syntax"),
        skip("c.jinja", "template does not render markup"),
    ];

    let (text, _) = report::render_format_pretty(&results, true, Verbosity::default());

    assert!(text.contains("0 file(s) checked"), "got: {text}");
    assert!(text.contains("2 Go/Helm template syntax"), "got: {text}");
    assert!(text.contains("1 template does not render markup"), "got: {text}");
}

/// A `DiscoveryReport` describing a pruned `test_apps/` tree plus two excluded
/// files, as a repo with `[discovery] exclude` would produce.
fn sample_discovery() -> DiscoveryReport {
    DiscoveryReport {
        excluded_files: 2,
        excluded_directories: 1,
        excluded_explicit: 0,
        rules: vec![
            ExcludedRule {
                pattern: "test_apps/**".to_string(),
                files: 0,
                directories: 1,
            },
            ExcludedRule {
                pattern: "**/*.tf".to_string(),
                files: 2,
                directories: 0,
            },
        ],
    }
}

/// `All formatted.` over a file set that discovery quietly pruned is the
/// reassuring lie this reporting exists to remove: the summary now says how much
/// was excluded, names the rules that did it, and — because a pruned directory
/// is never descended into — says outright that the files inside it are
/// uncounted rather than inventing a number.
#[test]
fn format_summary_reports_what_discovery_excluded() {
    owo_colors::set_override(false);
    let run = FormatRun {
        results: vec![FormatResult {
            path: PathBuf::from("src/clean.py"),
            changed: false,
            formatted: None,
            skipped: None,
            debug: None,
        }],
        discovery: sample_discovery(),
    };

    let (text, changed) = report::render_format_pretty_run(&run, true, Verbosity::default());

    assert_eq!(changed, 0);
    assert!(text.contains("All formatted."), "got: {text}");
    assert!(text.contains("1 file(s) checked"), "got: {text}");
    assert!(
        text.contains("2 file(s) and 1 director(ies) excluded by config"),
        "got: {text}"
    );
    assert!(text.contains("test_apps/** (1 dir(s))"), "got: {text}");
    assert!(text.contains("**/*.tf (2 file(s))"), "got: {text}");
    assert!(
        text.contains("excluded directories were not walked"),
        "the count's limits must be stated, not implied; got: {text}"
    );
}

/// The `--force-exclude` case: every path named on the command line was
/// excluded, so the run checked nothing. A green `All formatted.` there is the
/// original disease, so the headline changes and the note says why.
#[test]
fn format_summary_explains_a_run_that_checked_nothing() {
    owo_colors::set_override(false);
    let run = FormatRun {
        results: Vec::new(),
        discovery: DiscoveryReport {
            excluded_files: 1,
            excluded_directories: 0,
            excluded_explicit: 1,
            rules: vec![ExcludedRule {
                pattern: "**/*.tf".to_string(),
                files: 1,
                directories: 0,
            }],
        },
    };

    let (text, changed) = report::render_format_pretty_run(&run, true, Verbosity::default());

    assert_eq!(changed, 0);
    assert!(
        !text.contains("All formatted."),
        "a run that checked nothing must not read as a verified pass; got: {text}"
    );
    assert!(text.contains("Nothing was checked."), "got: {text}");
    assert!(text.contains("0 file(s) checked"), "got: {text}");
    assert!(
        text.contains("1 path(s) named on the command line were dropped by --force-exclude"),
        "got: {text}"
    );
}

/// Files that changed are reported as before, with the exclusion note appended —
/// a partial run is no more trustworthy than a clean one.
#[test]
fn format_summary_reports_exclusions_alongside_changed_files() {
    owo_colors::set_override(false);
    let run = FormatRun {
        results: sample_format_results(),
        discovery: sample_discovery(),
    };

    let (text, changed) = report::render_format_pretty_run(&run, true, Verbosity::default());

    assert_eq!(changed, 1);
    assert!(text.contains("1 file(s) will change of 2 file(s)"), "got: {text}");
    assert!(text.contains("test_apps/** (1 dir(s))"), "got: {text}");
}

/// `poly lint` carries the identical failure mode, and gets the identical
/// treatment: the count of what was linted plus what was pruned before it.
#[test]
fn lint_summary_reports_what_discovery_excluded() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: Vec::new(),
        checked: 969,
        discovery: sample_discovery(),
    };

    let (text, total) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert_eq!(total, 0);
    assert!(text.contains("No issues found."), "got: {text}");
    assert!(text.contains("969 file(s) linted"), "got: {text}");
    assert!(
        text.contains("2 file(s) and 1 director(ies) excluded by config"),
        "got: {text}"
    );
    assert!(text.contains("**/*.tf (2 file(s))"), "got: {text}");
}

/// A lint run that excluded everything reports no issues over no files — which
/// must not read as a clean bill of health.
#[test]
fn lint_summary_explains_a_run_that_linted_nothing() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: Vec::new(),
        checked: 0,
        discovery: sample_discovery(),
    };

    let (text, _) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert!(!text.contains("No issues found."), "got: {text}");
    assert!(text.contains("Nothing was linted."), "got: {text}");
    assert!(text.contains("0 file(s) linted"), "got: {text}");
}

/// With nothing excluded the summaries are unchanged — no note, no
/// qualification, no new noise on the overwhelmingly common path.
#[test]
fn summaries_stay_quiet_when_nothing_was_excluded() {
    owo_colors::set_override(false);
    let lint = LintRun {
        results: Vec::new(),
        checked: 12,
        discovery: DiscoveryReport::default(),
    };
    let (text, _) = report::render_lint_pretty_run(&lint, Verbosity::default());
    assert!(text.contains("No issues found. (12 file(s) linted)"), "got: {text}");
    assert!(!text.contains("excluded"), "got: {text}");

    let format = FormatRun {
        results: sample_format_results(),
        discovery: DiscoveryReport::default(),
    };
    let (text, _) = report::render_format_pretty_run(&format, true, Verbosity::default());
    assert!(!text.contains("excluded"), "got: {text}");
}

/// The results-only entry points keep their exact previous output, so an
/// existing caller sees no change until it opts into the run-level report.
#[test]
fn results_only_renderers_are_unchanged() {
    owo_colors::set_override(false);
    let (text, _) = report::render_lint_pretty(&[], Verbosity::default());
    assert_eq!(text, "No issues found.\n");

    let (text, _) = report::render_format_pretty(&[], true, Verbosity::default());
    assert_eq!(text, "All formatted. (0 file(s) checked)\n");
}
