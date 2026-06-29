//! Output rendering in three formats: `pretty` (colored, human-oriented),
//! `json` (`serde_json`), and `toon` (Token-Oriented Object Notation).
//!
//! Coloring goes through owo-colors' `if_supports_color`, which respects both
//! TTY detection and the global override set by `--no-color`. The `toon`
//! renderers fall back to JSON if TOON serialization fails so output is never
//! lost. The `pretty` renderers split into a `render_*` core that produces the
//! string and a `report_*` wrapper that prints it, so the rendered text can be
//! snapshot-tested.
//!
//! ## Verbosity contract
//!
//! [`Verbosity`] selects how much of each diagnostic the `pretty` renderers
//! show:
//! - **default** — one terse line per finding (`level  engine  code?  line:col?
//!   title`). `description`, `url`, and `metadata` are hidden.
//! - **`--verbose`** — additionally renders `description`, `url`, and any
//!   `metadata` as indented lines.
//! - **`--debug`** — additionally renders a dim per-file debug block (engine
//!   version, cache hit/miss, timing).
//!
//! For `json` / `toon` the full structured record is **always** emitted (serde
//! omits empty/`None` fields), so `--verbose` is a no-op there; `--debug` simply
//! causes the runner to attach the `debug` field, which then serializes.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stdout};

use crate::engine::Severity;
use crate::runner::{FormatResult, LintResult, RunDebug};

/// How much detail the human-oriented (`pretty`) renderers emit. `Copy` so it
/// threads cheaply through the renderers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Verbosity {
    /// Show `description`, `url`, and `metadata` for each finding.
    pub verbose: bool,
    /// Show the per-file debug block (engine version, cache hit/miss, timing).
    pub debug: bool,
}

impl Verbosity {
    /// Construct a [`Verbosity`] from the two flags.
    pub fn new(verbose: bool, debug: bool) -> Self {
        Self { verbose, debug }
    }
}

/// Format the colored severity label for a diagnostic.
fn severity_label(severity: Severity) -> String {
    match severity {
        Severity::Error => "error".if_supports_color(Stdout, |t| t.red()).to_string(),
        Severity::Warning => "warning"
            .if_supports_color(Stdout, |t| t.yellow())
            .to_string(),
        Severity::Info => "info".if_supports_color(Stdout, |t| t.blue()).to_string(),
        Severity::Hint => "hint".if_supports_color(Stdout, |t| t.cyan()).to_string(),
    }
}

/// Render the dim per-file debug block (engine version, cache hit/miss, timing).
fn render_debug_block(out: &mut String, debug: &RunDebug) {
    for e in &debug.engines {
        let status = if e.cache_hit { "cache hit" } else { "ran" };
        let line = format!(
            "[debug] {} v{}  {}  {:.2}ms",
            e.engine, e.version, status, e.duration_ms
        );
        let _ = writeln!(
            out,
            "      {}",
            line.if_supports_color(Stdout, |t| t.dimmed())
        );
    }
}

/// Build the human-oriented lint report as a string. By default one terse line
/// per diagnostic: `level  engine  code?  line:col?  title`. `--verbose` adds
/// `description`, `url`, and `metadata`; `--debug` adds a dim per-file debug
/// block. Returns the rendered text and the total diagnostic count.
pub fn render_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> (String, usize) {
    let mut out = String::new();
    let mut total = 0usize;
    for r in results {
        if r.diagnostics.is_empty() {
            // With --debug, surface timing even for files with no findings.
            if verbosity.debug
                && let Some(debug) = &r.debug
            {
                let _ = writeln!(
                    out,
                    "{}",
                    r.path.display().if_supports_color(Stdout, |t| t.bold())
                );
                render_debug_block(&mut out, debug);
            }
            continue;
        }
        let _ = writeln!(
            out,
            "{}",
            r.path.display().if_supports_color(Stdout, |t| t.bold())
        );
        for d in &r.diagnostics {
            total += 1;
            // Build the terse line from only the segments that are present.
            let mut segments: Vec<String> = Vec::with_capacity(5);
            segments.push(severity_label(d.severity));
            segments.push(
                d.engine
                    .if_supports_color(Stdout, |t| t.magenta())
                    .to_string(),
            );
            if let Some(code) = d.code.as_deref() {
                segments.push(code.if_supports_color(Stdout, |t| t.dimmed()).to_string());
            }
            if let Some(span) = &d.span {
                let loc = format!("{}:{}", span.start_line, span.start_col);
                segments.push(loc.if_supports_color(Stdout, |t| t.dimmed()).to_string());
            }
            segments.push(d.title.clone());
            let _ = writeln!(out, "  {}", segments.join("  "));

            if verbosity.verbose {
                if let Some(description) = d.description.as_deref() {
                    let _ = writeln!(
                        out,
                        "      {}",
                        description.if_supports_color(Stdout, |t| t.dimmed())
                    );
                }
                if let Some(url) = d.url.as_deref() {
                    let _ = writeln!(
                        out,
                        "      {}",
                        url.if_supports_color(Stdout, |t| t.dimmed())
                    );
                }
                for (key, value) in &d.metadata {
                    let _ = writeln!(
                        out,
                        "      {}",
                        format!("{key}={value}").if_supports_color(Stdout, |t| t.dimmed()),
                    );
                }
            }
        }
        if verbosity.debug
            && let Some(debug) = &r.debug
        {
            render_debug_block(&mut out, debug);
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
pub fn report_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> usize {
    let (text, total) = render_lint_pretty(results, verbosity);
    print!("{text}");
    total
}

/// Render lint results as pretty-printed JSON. The full structured record is
/// always emitted; serde omits `None`/empty fields. The `debug` field is present
/// only when the run collected it (`--debug`).
pub fn report_lint_json(results: &[LintResult]) -> String {
    serde_json::to_string_pretty(results).unwrap_or_else(|_| "[]".to_string())
}

/// Render lint results as TOON. Falls back to JSON if TOON serialization fails
/// so output is never silently dropped.
pub fn report_lint_toon(results: &[LintResult]) -> String {
    serde_toon::to_string(&results).unwrap_or_else(|_| report_lint_json(results))
}

/// Build the human-oriented format report as a string. `check` selects
/// "would reformat" vs "reformatted" phrasing. `--debug` appends a dim per-file
/// debug block (engine version, cache hit/miss, timing). Returns the rendered
/// text and the number of changed files.
pub fn render_format_pretty(
    results: &[FormatResult],
    check: bool,
    verbosity: Verbosity,
) -> (String, usize) {
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
    if verbosity.debug {
        for r in results {
            if let Some(debug) = &r.debug {
                let _ = writeln!(
                    out,
                    "{}",
                    r.path.display().if_supports_color(Stdout, |t| t.bold())
                );
                render_debug_block(&mut out, debug);
            }
        }
    }
    (out, n)
}

/// Print the human-oriented format report to stdout. Returns the number of
/// changed files.
pub fn report_format_pretty(results: &[FormatResult], check: bool, verbosity: Verbosity) -> usize {
    let (text, n) = render_format_pretty(results, check, verbosity);
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
