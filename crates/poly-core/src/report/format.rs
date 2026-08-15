//! The human-oriented (`pretty`) format renderers: the changed-file lines, the
//! qualified summary, and the block naming the files the formatter could not
//! process.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stderr, Stream::Stdout};

use super::notes::{
    exclusion_clause, push_discovery_note, push_skip_note, skipped_clause, skips_from_results, unrecognized_clause,
};
use super::shared::{Verbosity, render_debug_block, strip_ansi};
use crate::discover::DiscoveryReport;
use crate::runner::{FormatError, FormatResult, FormatRun, SkippedFile};

/// Echo format failures to stderr, so a machine-readable run still tells a
/// human watching the terminal that files went unchecked.
///
/// The counterpart to [`eprint_lint_errors`](crate::report::eprint_lint_errors): under `--format json`/`--toon`
/// stdout must stay a single valid document, so the failures go to stderr.
/// Without this, a `poly fmt --format json` failure surfaced only as a tracing
/// `WARN`, which the lint path has never relied on.
pub fn eprint_format_errors(errors: &[FormatError]) {
    if errors.is_empty() {
        return;
    }
    let plain = strip_ansi(&render_format_errors(errors));
    eprint!("{}", plain.if_supports_color(Stderr, |t| t.red()));
}

/// Build the human-oriented format report as a string. `check` selects
/// "would reformat" vs "reformatted" phrasing. `--debug` appends a dim per-file
/// debug block (engine version, cache hit/miss, timing). Returns the rendered
/// text and the number of changed files.
pub fn render_format_pretty(results: &[FormatResult], check: bool, verbosity: Verbosity) -> (String, usize) {
    render_format_core(
        results,
        &skips_from_results(results),
        &DiscoveryReport::default(),
        check,
        verbosity,
    )
}

/// [`render_format_pretty`] over a whole [`FormatRun`], so the summary can say
/// what it skipped and what discovery excluded before the checked files were
/// reached.
pub fn render_format_pretty_run(run: &FormatRun, check: bool, verbosity: Verbosity) -> (String, usize) {
    let (mut out, changed) = render_format_core(&run.results, &run.skipped, &run.discovery, check, verbosity);
    out.push_str(&render_format_errors(&run.errors));
    (out, changed)
}

/// Render the files the formatter could not process, naming each path.
///
/// A file whose engine errored has not been verified, so it must be visible and
/// it must not be folded into the pass/fail count for *formatted* files — the
/// caller exits 2 on these, distinct from exit 1 ("files changed").
pub fn render_format_errors(errors: &[FormatError]) -> String {
    if errors.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for error in errors {
        let _ = writeln!(
            out,
            "{} {}: {}",
            "error".if_supports_color(Stdout, |t| t.red()),
            error.path.display(),
            error.message
        );
    }
    let _ = writeln!(
        out,
        "{}",
        format!("{} file(s) could not be formatted and were NOT checked.", errors.len())
            .if_supports_color(Stdout, |t| t.red())
    );
    out
}

/// Shared body of the format renderers.
///
/// `skipped` is the run-level skip set, which is a superset of the per-result
/// [`FormatResult::skipped`] reasons: an explicitly named path that no engine
/// covers never produces a [`FormatResult`] at all, so it can only be counted
/// from here.
fn render_format_core(
    results: &[FormatResult],
    skipped: &[SkippedFile],
    discovery: &DiscoveryReport,
    check: bool,
    verbosity: Verbosity,
) -> (String, usize) {
    let mut out = String::new();
    let changed: Vec<&FormatResult> = results.iter().filter(|r| r.changed).collect();
    for r in &changed {
        let verb = if check { "would reformat" } else { "reformatted" };
        let _ = writeln!(
            out,
            "{} {}",
            verb.if_supports_color(Stdout, |t| t.yellow()),
            r.path.display()
        );
    }
    let scanned = results.len();
    let declined = results.iter().filter(|r| r.skipped.is_some()).count();
    let checked = scanned - declined;
    let n = changed.len();
    if n == 0 {
        // `scanned` counted files that were discovered and routed, including
        // those every backend declined — so a skipped file read exactly like a
        // verified one. Report what was actually inspected, and name the skips.
        let mut tail = format!("{checked} file(s) checked");
        if let Some(clause) = skipped_clause(skipped) {
            let _ = write!(tail, ", {clause}");
        }
        if let Some(clause) = exclusion_clause(discovery) {
            let _ = write!(tail, ", {clause}");
        }
        if let Some(clause) = unrecognized_clause(discovery) {
            let _ = write!(tail, ", {clause}");
        }
        // A green "All formatted." over an empty file set is the reassuring lie
        // this feature exists to remove: when the exclude set is the reason
        // nothing was checked, say so instead.
        let headline = if checked == 0 && (discovery.has_notes() || !skipped.is_empty()) {
            "Nothing was checked."
                .if_supports_color(Stdout, |t| t.yellow())
                .to_string()
        } else {
            "All formatted.".if_supports_color(Stdout, |t| t.green()).to_string()
        };
        let _ = writeln!(out, "{headline} ({tail})");
    } else {
        let phrase = if check {
            format!("{n} file(s) will change")
        } else {
            format!("{n} changed")
        };
        let mut tail = format!("of {scanned} file(s)");
        // A partial result is no more trustworthy than a clean one: qualify it
        // with the same skip and exclusion accounting. Reporting drift used to
        // drop the skip clause entirely, so a run that both changed files and
        // declined others said nothing about the second half.
        let mut qualifiers: Vec<String> = Vec::with_capacity(3);
        if let Some(clause) = skipped_clause(skipped) {
            qualifiers.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            qualifiers.push(clause);
        }
        if let Some(clause) = unrecognized_clause(discovery) {
            qualifiers.push(clause);
        }
        if !qualifiers.is_empty() {
            let _ = write!(tail, " ({})", qualifiers.join(", "));
        }
        let _ = writeln!(out, "\n{} {tail}", phrase.if_supports_color(Stdout, |t| t.yellow()));
    }
    push_discovery_note(&mut out, discovery);
    push_skip_note(&mut out, skipped, verbosity.verbose);
    if verbosity.debug {
        for r in results {
            if let Some(debug) = &r.debug {
                let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
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

/// Print the human-oriented format report for a whole [`FormatRun`] to stdout,
/// so the summary is qualified by what discovery excluded. Returns the number of
/// changed files.
pub fn report_format_pretty_run(run: &FormatRun, check: bool, verbosity: Verbosity) -> usize {
    let (text, n) = render_format_pretty_run(run, check, verbosity);
    print!("{text}");
    n
}
