//! Output rendering in three formats: `pretty` (colored, human-oriented),
//! `json` (`serde_json`), and `toon` (Token-Oriented Object Notation).
//!
//! Coloring goes through owo-colors' `if_supports_color`, which respects both
//! TTY detection and the global override set by `--no-color`. The `toon`
//! renderers fall back to JSON if TOON serialization fails so output is never
//! lost. The `pretty` renderers split into a `render_*` core that produces the
//! string and a `report_*` wrapper that prints it, so the rendered text can be
//! snapshot-tested.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stdout};

use crate::engine::Severity;
use crate::runner::{FormatResult, LintResult};

/// Build the human-oriented lint report as a string, one row per diagnostic
/// showing the full envelope: `line:col`, severity, engine, code, message, and
/// any metadata `key=value` extras. Returns the rendered text and the total
/// diagnostic count.
pub fn render_lint_pretty(results: &[LintResult]) -> (String, usize) {
    let mut out = String::new();
    let mut total = 0usize;
    for r in results {
        if r.diagnostics.is_empty() {
            continue;
        }
        let _ = writeln!(
            out,
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
            let _ = writeln!(
                out,
                "  {}:{}  {}  {}  {}  {}",
                line.if_supports_color(Stdout, |t| t.dimmed()),
                col.if_supports_color(Stdout, |t| t.dimmed()),
                sev,
                d.engine.if_supports_color(Stdout, |t| t.magenta()),
                code.if_supports_color(Stdout, |t| t.dimmed()),
                d.message,
            );
            for (key, value) in &d.metadata {
                let _ = writeln!(
                    out,
                    "      {}",
                    format!("{key}={value}").if_supports_color(Stdout, |t| t.dimmed()),
                );
            }
        }
    }
    if total == 0 {
        let _ = writeln!(
            out,
            "{}",
            "No issues found.".if_supports_color(Stdout, |t| t.green())
        );
    } else {
        let _ = writeln!(
            out,
            "\n{}",
            format!("{total} issue(s) found.").if_supports_color(Stdout, |t| t.red())
        );
    }
    (out, total)
}

/// Print the human-oriented lint report to stdout. Returns the total
/// diagnostic count.
pub fn report_lint_pretty(results: &[LintResult]) -> usize {
    let (text, total) = render_lint_pretty(results);
    print!("{text}");
    total
}

/// Render lint results as pretty-printed JSON.
pub fn report_lint_json(results: &[LintResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render lint results as TOON. Falls back to JSON if TOON serialization fails
/// so output is never silently dropped.
pub fn report_lint_toon(results: &[LintResult]) -> String {
    serde_toon::to_string(&results).unwrap_or_else(|_| report_lint_json(results))
}

/// Build the human-oriented format report as a string. `check` selects
/// "would reformat" vs "reformatted" phrasing. Returns the rendered text and
/// the number of changed files.
pub fn render_format_pretty(results: &[FormatResult], check: bool) -> (String, usize) {
    let mut out = String::new();
    let changed: Vec<&FormatResult> = results.iter().filter(|r| r.changed).collect();
    for r in &changed {
        let verb = if check {
            "would reformat"
        } else {
            "reformatted"
        };
        let _ = writeln!(
            out,
            "{} {}",
            verb.if_supports_color(Stdout, |t| t.yellow()),
            r.path.display()
        );
    }
    let scanned = results.len();
    let n = changed.len();
    if n == 0 {
        let _ = writeln!(
            out,
            "{} ({scanned} file(s) scanned)",
            "All formatted.".if_supports_color(Stdout, |t| t.green())
        );
    } else {
        let _ = writeln!(
            out,
            "\n{} of {scanned} file(s)",
            format!("{n} changed").if_supports_color(Stdout, |t| t.yellow())
        );
    }
    (out, n)
}

/// Print the human-oriented format report to stdout. Returns the number of
/// changed files.
pub fn report_format_pretty(results: &[FormatResult], check: bool) -> usize {
    let (text, n) = render_format_pretty(results, check);
    print!("{text}");
    n
}

/// Render format results as pretty-printed JSON.
pub fn report_format_json(results: &[FormatResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render format results as TOON. Falls back to JSON if TOON serialization
/// fails so output is never silently dropped.
pub fn report_format_toon(results: &[FormatResult]) -> String {
    serde_toon::to_string(&results).unwrap_or_else(|_| report_format_json(results))
}
