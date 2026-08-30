//! The human-oriented (`pretty`) format renderers: the changed-file lines, the
//! qualified summary, and the block naming the files the formatter could not
//! process.

use std::fmt::Write as _;

use super::layout::files;
use super::notes::{push_discovery_note, push_skip_note, qualification_lines, skips_from_results};
use super::shared::{Verbosity, render_debug_block, strip_ansi};
use super::theme::Theme;
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
    eprint!("{}", Theme::STDERR.failure(strip_ansi(&render_format_errors(errors))));
}

/// Build the human-oriented format report as a string. `check` selects
/// "would reformat" vs "reformatted" phrasing. `--debug` appends a dim per-file
/// debug block (engine version, cache hit/miss, timing). Returns the rendered
/// text and the number of changed files.
pub fn render_format_pretty(results: &[FormatResult], check: bool, verbosity: Verbosity) -> (String, usize) {
    render_format_core(
        results,
        &skips_from_results(results),
        &[],
        &DiscoveryReport::default(),
        check,
        verbosity,
    )
}

/// [`render_format_pretty`] over a whole [`FormatRun`], so the summary can say
/// what it skipped and what discovery excluded before the checked files were
/// reached.
pub fn render_format_pretty_run(run: &FormatRun, check: bool, verbosity: Verbosity) -> (String, usize) {
    render_format_core(
        &run.results,
        &run.skipped,
        &run.errors,
        &run.discovery,
        check,
        verbosity,
    )
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
    let theme = Theme::STDOUT;
    let mut out = String::new();
    for error in errors {
        let _ = writeln!(
            out,
            "{} {}: {}",
            theme.failure("error"),
            error.path.display(),
            error.message
        );
    }
    let _ = writeln!(
        out,
        "{}",
        theme.failure(format!(
            "{} could not be formatted and {} NOT checked",
            files(errors.len()),
            if errors.len() == 1 { "was" } else { "were" }
        ))
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
    errors: &[FormatError],
    discovery: &DiscoveryReport,
    check: bool,
    verbosity: Verbosity,
) -> (String, usize) {
    let theme = Theme::STDOUT;
    let mut out = String::new();
    let changed: Vec<&FormatResult> = results.iter().filter(|r| r.changed).collect();
    for r in &changed {
        let verb = if check { "would reformat" } else { "reformatted" };
        let _ = writeln!(out, "{} {}", theme.skipped(verb), r.path.display());
    }
    if verbosity.shows_debug_blocks() {
        for r in results {
            if let Some(debug) = &r.debug {
                let _ = writeln!(out, "{}", theme.heading(r.path.display()));
                render_debug_block(&mut out, debug);
            }
        }
    }
    let scanned = results.len();
    let declined = results.iter().filter(|r| r.skipped.is_some()).count();
    // `scanned` counts files that were discovered and routed, including those
    // every backend declined — so a skipped file read exactly like a verified
    // one. Report what was actually inspected.
    let checked = scanned - declined;
    let n = changed.len();

    // Failures first, then the qualification, then the verdict — the same
    // reading order the lint report uses, so the answer is always the last thing
    // on screen.
    out.push_str(&render_format_errors(errors));
    push_discovery_note(&mut out, discovery, verbosity);
    push_skip_note(&mut out, skipped, verbosity);

    // A green "All formatted." over an empty file set is the reassuring lie this
    // accounting exists to remove: when the exclude set is the reason nothing was
    // checked, say so instead.
    let nothing_checked = checked == 0 && (discovery.has_notes() || !skipped.is_empty());
    let headline = match (n > 0, nothing_checked) {
        (true, _) if check => theme.skipped(format!("{} will change.", files(n))),
        (true, _) => theme.skipped(format!("{} reformatted.", files(n))),
        (false, true) => theme.skipped("Nothing was checked."),
        (false, false) => theme.success("All formatted."),
    };
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = writeln!(out, "{headline}");
    for line in qualification_lines(Some(checked), "checked", skipped, discovery) {
        let _ = writeln!(out, "{line}");
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
