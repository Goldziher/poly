//! Output rendering: colored human-readable and machine-readable JSON.
//!
//! Coloring goes through owo-colors' `if_supports_color`, which respects both
//! TTY detection and the global override set by `--no-color`.

use owo_colors::{OwoColorize, Stream::Stdout};

use crate::engine::Severity;
use crate::runner::{FormatResult, LintResult};

/// Render lint results for humans. Returns the total diagnostic count.
pub fn report_lint_human(results: &[LintResult]) -> usize {
    let mut total = 0usize;
    for r in results {
        if r.diagnostics.is_empty() {
            continue;
        }
        println!(
            "{}",
            r.path.display().if_supports_color(Stdout, |t| t.bold())
        );
        for d in &r.diagnostics {
            total += 1;
            let (line, col) = d
                .span
                .as_ref()
                .map(|s| (s.start_line, s.start_col))
                .unwrap_or((0, 0));
            let sev = match d.severity {
                Severity::Error => "error".if_supports_color(Stdout, |t| t.red()).to_string(),
                Severity::Warning => "warning"
                    .if_supports_color(Stdout, |t| t.yellow())
                    .to_string(),
                Severity::Info => "info".if_supports_color(Stdout, |t| t.blue()).to_string(),
                Severity::Hint => "hint".if_supports_color(Stdout, |t| t.cyan()).to_string(),
            };
            let code = d.code.as_deref().unwrap_or("");
            println!(
                "  {}:{}  {}  {}  {}",
                line.if_supports_color(Stdout, |t| t.dimmed()),
                col.if_supports_color(Stdout, |t| t.dimmed()),
                sev,
                d.message,
                code.if_supports_color(Stdout, |t| t.dimmed()),
            );
        }
    }
    if total == 0 {
        println!(
            "{}",
            "No issues found.".if_supports_color(Stdout, |t| t.green())
        );
    } else {
        println!(
            "\n{}",
            format!("{total} issue(s) found.").if_supports_color(Stdout, |t| t.red())
        );
    }
    total
}

/// Render lint results as JSON.
pub fn report_lint_json(results: &[LintResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render format results for humans. `check` selects "would reformat" vs
/// "reformatted" phrasing. Returns the number of changed files.
pub fn report_format_human(results: &[FormatResult], check: bool) -> usize {
    let changed: Vec<&FormatResult> = results.iter().filter(|r| r.changed).collect();
    for r in &changed {
        let verb = if check {
            "would reformat"
        } else {
            "reformatted"
        };
        println!(
            "{} {}",
            verb.if_supports_color(Stdout, |t| t.yellow()),
            r.path.display()
        );
    }
    let scanned = results.len();
    let n = changed.len();
    if n == 0 {
        println!(
            "{} ({scanned} file(s) scanned)",
            "All formatted.".if_supports_color(Stdout, |t| t.green())
        );
    } else {
        println!(
            "\n{} of {scanned} file(s)",
            format!("{n} changed").if_supports_color(Stdout, |t| t.yellow())
        );
    }
    n
}

/// Render format results as JSON.
pub fn report_format_json(results: &[FormatResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}
