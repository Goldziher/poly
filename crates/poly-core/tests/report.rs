//! Snapshot tests for the three output renderers (pretty / json / toon) over a
//! synthetic result set that includes a diagnostic with populated `metadata`
//! and one without. Color is forced off so the `pretty` snapshot is stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use poly_core::report::{self, Verbosity};
use poly_core::runner::{EngineDebug, FormatResult, FormatRun, LintError, LintResult, LintRun, RunDebug, SkippedFile};
use poly_core::{Diagnostic, DiscoveryReport, Edit, ExcludedRule, Severity, Span};

fn sample_lint_results() -> Vec<LintResult> {
    let mut metadata = BTreeMap::new();
    metadata.insert("category".to_string(), "style".to_string());
    metadata.insert("url".to_string(), "https://example.test/rules/E501".to_string());

    vec![
        LintResult {
            path: PathBuf::from("src/main.py"),
            fix_withheld_generated: false,
            fixed: 0,
            skipped: None,
            error: None,
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
            fixed: 0,
            skipped: None,
            error: None,
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
        fixed: 0,
        skipped: None,
        error: None,
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
        fixed: 0,
        skipped: None,
        error: None,
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
        fixed: 0,
        skipped: None,
        error: None,
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
                ..ExcludedRule::default()
            },
            ExcludedRule {
                pattern: "**/*.tf".to_string(),
                files: 2,
                directories: 0,
                ..ExcludedRule::default()
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
        skipped: Vec::new(),
        errors: Vec::new(),
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
        skipped: Vec::new(),
        errors: Vec::new(),
        results: Vec::new(),
        discovery: DiscoveryReport {
            excluded_files: 1,
            excluded_directories: 0,
            excluded_explicit: 1,
            rules: vec![ExcludedRule {
                pattern: "**/*.tf".to_string(),
                files: 1,
                directories: 0,
                ..ExcludedRule::default()
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
        skipped: Vec::new(),
        errors: Vec::new(),
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
        errors: Vec::new(),
        skipped: Vec::new(),
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
        errors: Vec::new(),
        skipped: Vec::new(),
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
        errors: Vec::new(),
        skipped: Vec::new(),
        results: Vec::new(),
        checked: 12,
        discovery: DiscoveryReport::default(),
    };
    let (text, _) = report::render_lint_pretty_run(&lint, Verbosity::default());
    assert!(text.contains("No issues found. (12 file(s) linted)"), "got: {text}");
    assert!(!text.contains("excluded"), "got: {text}");

    let format = FormatRun {
        skipped: Vec::new(),
        errors: Vec::new(),
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

/// A path named on the command line that no engine covers is the reported
/// defect: `poly lint App.csproj` said `No issues found. (0 file(s) linted)` and
/// exited 0. The count is now qualified and the path is named — the reporter's
/// "'No issues found' is a false reassurance here; nothing was examined".
#[test]
fn lint_summary_names_paths_that_matched_no_engine() {
    owo_colors::set_override(false);
    let run = LintRun {
        errors: Vec::new(),
        results: Vec::new(),
        checked: 4,
        skipped: vec![SkippedFile {
            path: PathBuf::from("packages/csharp/App.csproj"),
            reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let (text, _) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert!(text.contains("4 file(s) linted"), "got: {text}");
    assert!(
        text.contains("1 skipped (no matching engine for this file type)"),
        "the count must say what it skipped, got: {text}"
    );
    assert!(
        text.contains("packages/csharp/App.csproj"),
        "the path must be named, not left to bisection, got: {text}"
    );
}

/// A run that reports drift used to drop the skip accounting entirely, so a run
/// that both changed files and declined others said nothing about the second
/// half.
#[test]
fn format_summary_reports_skips_alongside_changed_files() {
    owo_colors::set_override(false);
    let run = FormatRun {
        errors: Vec::new(),
        results: sample_format_results(),
        skipped: vec![SkippedFile {
            path: PathBuf::from("gen.py"),
            reason: "hash-stamped generated file".to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let (text, changed) = report::render_format_pretty_run(&run, true, Verbosity::default());

    assert_eq!(changed, 1);
    assert!(text.contains("1 skipped (hash-stamped generated file)"), "got: {text}");
    assert!(text.contains("skipped gen.py"), "got: {text}");
}

/// The listing is bounded by default so a repo with hundreds of generated files
/// does not bury its findings, and `--verbose` lifts the cap for someone who
/// wants the whole set without switching to JSON.
#[test]
fn skip_listing_is_capped_by_default_and_uncapped_under_verbose() {
    owo_colors::set_override(false);
    let skipped: Vec<SkippedFile> = (0..25)
        .map(|i| SkippedFile {
            path: PathBuf::from(format!("gen{i}.py")),
            reason: "hash-stamped generated file".to_string(),
        })
        .collect();

    let capped = report::render_skip_note(&skipped, false).expect("a note for 25 skips");
    assert!(capped.contains("gen0.py"), "got: {capped}");
    assert!(
        !capped.contains("gen24.py"),
        "the default view is capped, got: {capped}"
    );
    assert!(capped.contains("and 5 more skipped file(s)"), "got: {capped}");

    let full = report::render_skip_note(&skipped, true).expect("a note for 25 skips");
    assert!(full.contains("gen24.py"), "--verbose must list them all, got: {full}");
    assert!(!full.contains("more skipped file(s)"), "got: {full}");
}

/// Nothing skipped, nothing said.
#[test]
fn skip_note_is_absent_when_nothing_was_skipped() {
    assert!(report::render_skip_note(&[], true).is_none());
}

/// The reporter's stronger ask: assert on the skipped *set* structurally rather
/// than reconstructing it from a heuristic and scraping the human summary. The
/// document stays an array of per-file records, so existing consumers are
/// unaffected — skipped files simply appear with an empty `diagnostics` list.
#[test]
fn lint_json_run_appends_skipped_paths_as_entries() {
    let run = LintRun {
        errors: Vec::new(),
        results: sample_lint_results(),
        checked: 2,
        skipped: vec![SkippedFile {
            path: PathBuf::from("App.csproj"),
            reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let json = report::report_lint_json_run(&run);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let entries = value.as_array().expect("top level stays an array");
    assert_eq!(entries.len(), 3, "two results plus the skipped path: {json}");
    let skipped = entries.last().expect("the appended entry");
    assert_eq!(skipped["path"], "App.csproj");
    assert_eq!(skipped["skipped"], poly_core::runner::NO_ENGINE_SKIP);
    assert_eq!(
        skipped["diagnostics"].as_array().map(Vec::len),
        Some(0),
        "a skipped file has no findings to report: {json}"
    );
}

/// A file a backend declined already has a `FormatResult` carrying its reason,
/// so it must not be duplicated by the run-level skip list.
#[test]
fn format_json_run_does_not_duplicate_declined_files() {
    let declined = FormatResult {
        path: PathBuf::from("Taskfile.yaml"),
        changed: false,
        formatted: None,
        skipped: Some("Go/Helm template syntax".to_string()),
        debug: None,
    };
    let run = FormatRun {
        errors: Vec::new(),
        results: vec![declined],
        skipped: vec![
            SkippedFile {
                path: PathBuf::from("Taskfile.yaml"),
                reason: "Go/Helm template syntax".to_string(),
            },
            SkippedFile {
                path: PathBuf::from("App.csproj"),
                reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
            },
        ],
        discovery: DiscoveryReport::default(),
    };

    let json = report::report_format_json_run(&run);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let entries = value.as_array().expect("top level stays an array");
    assert_eq!(entries.len(), 2, "the declined file appears exactly once: {json}");
    assert_eq!(entries[1]["path"], "App.csproj");
    assert_eq!(entries[1]["skipped"], poly_core::runner::NO_ENGINE_SKIP);
}

/// The reporter's exact objection: "'No issues found' is a false reassurance
/// here. Nothing was examined." A run whose only path was skipped examined
/// nothing, so the headline must not read as a verified pass — regardless of
/// whether the file set was emptied by the exclude rules or by the skip.
#[test]
fn a_run_whose_only_file_was_skipped_does_not_read_as_clean() {
    owo_colors::set_override(false);
    let skipped = vec![SkippedFile {
        path: PathBuf::from("App.csproj"),
        reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
    }];

    let lint = LintRun {
        errors: Vec::new(),
        results: Vec::new(),
        checked: 0,
        skipped: skipped.clone(),
        discovery: DiscoveryReport::default(),
    };
    let (text, _) = report::render_lint_pretty_run(&lint, Verbosity::default());
    assert!(!text.contains("No issues found."), "got: {text}");
    assert!(text.contains("Nothing was linted."), "got: {text}");

    let format = FormatRun {
        errors: Vec::new(),
        results: Vec::new(),
        skipped,
        discovery: DiscoveryReport::default(),
    };
    let (text, _) = report::render_format_pretty_run(&format, true, Verbosity::default());
    assert!(!text.contains("All formatted."), "got: {text}");
    assert!(text.contains("Nothing was checked."), "got: {text}");
}

/// A file whose engine *errored* is a third outcome, distinct from a finding and
/// from a skip: it was accepted for linting and then not linted. The summary must
/// say so rather than reporting the run as clean — the exact false pass the
/// per-file `filter_map` used to produce.
#[test]
fn lint_summary_names_a_file_the_engine_could_not_process() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: Vec::new(),
        checked: 1,
        skipped: Vec::new(),
        errors: vec![LintError {
            path: PathBuf::from("bad.py"),
            message: "stream did not contain valid UTF-8".to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let (text, total) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert_eq!(total, 0, "an unreadable file produces no diagnostics");
    assert_eq!(
        text,
        concat!(
            "Lint did not complete. (1 file(s) linted)\n",
            "error bad.py: stream did not contain valid UTF-8\n",
            "1 file(s) could not be linted and were NOT checked.\n",
        )
    );
}

/// An error and a skip are reported apart: the skip keeps its own note and its
/// own count, and the errored file appears in neither. Folding one into the other
/// would recreate the defect in a new costume — a skip is poly declining, an
/// error is poly failing.
#[test]
fn lint_summary_keeps_errors_and_skips_apart() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: Vec::new(),
        checked: 1,
        skipped: vec![SkippedFile {
            path: PathBuf::from("App.csproj"),
            reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
        }],
        errors: vec![LintError {
            path: PathBuf::from("bad.py"),
            message: "stream did not contain valid UTF-8".to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let (text, _) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert_eq!(
        text,
        concat!(
            "Lint did not complete. (1 file(s) linted, 1 skipped (no matching engine for this file type))\n",
            "  skipped App.csproj: no matching engine for this file type\n",
            "error bad.py: stream did not contain valid UTF-8\n",
            "1 file(s) could not be linted and were NOT checked.\n",
        )
    );
}

/// `--fix` still reports what it rewrote — a run that both fixed and failed owes
/// the caller both halves — but the headline no longer reads as a successful run.
#[test]
fn lint_summary_does_not_let_fixes_imply_success_when_a_file_errored() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: vec![LintResult {
            path: PathBuf::from("ok.py"),
            diagnostics: Vec::new(),
            fix_withheld_generated: false,
            fixed: 2,
            skipped: None,
            error: None,
            debug: None,
        }],
        checked: 1,
        skipped: Vec::new(),
        errors: vec![LintError {
            path: PathBuf::from("bad.py"),
            message: "stream did not contain valid UTF-8".to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let (text, _) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert_eq!(
        text,
        concat!(
            "Lint did not complete. (1 file(s) linted)\n",
            "Fixed 2 issue(s) in 1 file(s).\n",
            "error bad.py: stream did not contain valid UTF-8\n",
            "1 file(s) could not be linted and were NOT checked.\n",
        )
    );
}

/// A run with no errors renders exactly as before — the clean path gains nothing.
#[test]
fn lint_summary_without_errors_is_unchanged() {
    owo_colors::set_override(false);
    let run = LintRun {
        results: Vec::new(),
        checked: 3,
        skipped: Vec::new(),
        errors: Vec::new(),
        discovery: DiscoveryReport::default(),
    };

    let (text, _) = report::render_lint_pretty_run(&run, Verbosity::default());

    assert_eq!(text, "No issues found. (3 file(s) linted)\n");
}

/// The error travels structurally too, in the same top-level array as everything
/// else, and in its own field: a consumer must be able to tell an errored file
/// from a skipped one without parsing prose.
#[test]
fn lint_json_run_carries_errors_separately_from_skips() {
    let run = LintRun {
        results: Vec::new(),
        checked: 1,
        skipped: vec![SkippedFile {
            path: PathBuf::from("App.csproj"),
            reason: poly_core::runner::NO_ENGINE_SKIP.to_string(),
        }],
        errors: vec![LintError {
            path: PathBuf::from("bad.py"),
            message: "stream did not contain valid UTF-8".to_string(),
        }],
        discovery: DiscoveryReport::default(),
    };

    let json = report::report_lint_json_run(&run);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let entries = value.as_array().expect("top level stays an array");
    assert_eq!(entries.len(), 2, "the skipped path and the errored one: {json}");

    let skipped = entries
        .iter()
        .find(|entry| entry["path"] == "App.csproj")
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(skipped["skipped"], poly_core::runner::NO_ENGINE_SKIP);
    assert!(skipped["error"].is_null(), "a skip is not an error: {json}");

    let errored = entries
        .iter()
        .find(|entry| entry["path"] == "bad.py")
        .unwrap_or_else(|| panic!("{json}"));
    assert_eq!(errored["error"], "stream did not contain valid UTF-8");
    assert!(errored["skipped"].is_null(), "an error is not a skip: {json}");
    assert_eq!(
        errored["diagnostics"].as_array().map(Vec::len),
        Some(0),
        "a file that was never read has no findings: {json}"
    );
}

/// The stderr echo used under `--format json`, where stdout must stay a single
/// valid document.
#[test]
fn lint_error_note_names_every_failing_path() {
    owo_colors::set_override(false);
    let errors = vec![
        LintError {
            path: PathBuf::from("a.py"),
            message: "boom".to_string(),
        },
        LintError {
            path: PathBuf::from("b.py"),
            message: "bang".to_string(),
        },
    ];

    assert_eq!(
        report::render_lint_errors(&errors),
        concat!(
            "error a.py: boom\n",
            "error b.py: bang\n",
            "2 file(s) could not be linted and were NOT checked.\n",
        )
    );
    assert_eq!(report::render_lint_errors(&[]), "", "no errors, no text");
}
