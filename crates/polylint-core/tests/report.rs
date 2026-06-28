//! Snapshot tests for the three output renderers (pretty / json / toon) over a
//! synthetic result set that includes a diagnostic with populated `metadata`
//! and one without. Color is forced off so the `pretty` snapshot is stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use polylint_core::report;
use polylint_core::runner::{FormatResult, LintResult};
use polylint_core::{Diagnostic, Severity, Span};

fn sample_lint_results() -> Vec<LintResult> {
    let mut metadata = BTreeMap::new();
    metadata.insert("category".to_string(), "style".to_string());
    metadata.insert(
        "url".to_string(),
        "https://example.test/rules/E501".to_string(),
    );

    vec![
        LintResult {
            path: PathBuf::from("src/main.py"),
            diagnostics: vec![
                Diagnostic {
                    engine: "ruff".to_string(),
                    code: Some("E501".to_string()),
                    severity: Severity::Warning,
                    message: "line too long".to_string(),
                    span: Some(Span {
                        start_line: 12,
                        start_col: 80,
                        end_line: 12,
                        end_col: 95,
                    }),
                    fix: vec![],
                    metadata,
                },
                Diagnostic {
                    engine: "ruff".to_string(),
                    code: None,
                    severity: Severity::Error,
                    message: "syntax error".to_string(),
                    span: Some(Span {
                        start_line: 1,
                        start_col: 1,
                        end_line: 1,
                        end_col: 1,
                    }),
                    fix: vec![],
                    metadata: BTreeMap::new(),
                },
            ],
        },
        LintResult {
            path: PathBuf::from("src/clean.py"),
            diagnostics: vec![],
        },
    ]
}

fn sample_format_results() -> Vec<FormatResult> {
    vec![
        FormatResult {
            path: PathBuf::from("src/main.py"),
            changed: true,
            formatted: Some("formatted".to_string()),
        },
        FormatResult {
            path: PathBuf::from("src/clean.py"),
            changed: false,
            formatted: None,
        },
    ]
}

#[test]
fn lint_pretty_renders_full_envelope_with_metadata() {
    owo_colors::set_override(false);
    let (text, total) = report::render_lint_pretty(&sample_lint_results());
    assert_eq!(total, 2, "two diagnostics across the result set");
    insta::assert_snapshot!("lint_pretty", text);
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
    let (text, changed) = report::render_format_pretty(&sample_format_results(), false);
    assert_eq!(changed, 1, "one file changed");
    insta::assert_snapshot!("format_pretty", text);
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
