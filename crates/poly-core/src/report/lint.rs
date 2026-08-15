//! The human-oriented (`pretty`) lint renderers: the per-finding lines, the
//! qualified summary, the fix accounting, and the block naming the files the
//! linter could not process.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stderr, Stream::Stdout};

use super::notes::{exclusion_clause, push_discovery_note, push_skip_note, skipped_clause, unrecognized_clause};
use super::shared::{Verbosity, render_debug_block, severity_label, strip_ansi};
use crate::discover::DiscoveryReport;
use crate::runner::{LintError, LintResult, LintRun, SkippedFile};

/// Build the human-oriented lint report as a string. By default one terse line
/// per diagnostic: `level  engine  code?  line:col?  title`. `--verbose` adds
/// `description`, `url`, and `metadata`; `--debug` adds a dim per-file debug
/// block. Returns the rendered text and the total diagnostic count.
///
/// The summary is unqualified: it can say "No issues found." but not how much was
/// looked at. Prefer [`render_lint_pretty_run`], which does both.
pub fn render_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> (String, usize) {
    render_lint_core(results, None, &[], &[], &DiscoveryReport::default(), verbosity)
}

/// [`render_lint_pretty`] over a whole [`LintRun`], so the summary can state how
/// many files were linted, which ones it skipped, which ones failed, and what
/// discovery excluded before them.
pub fn render_lint_pretty_run(run: &LintRun, verbosity: Verbosity) -> (String, usize) {
    render_lint_core(
        &run.results,
        Some(run.checked),
        &run.skipped,
        &run.errors,
        &run.discovery,
        verbosity,
    )
}

/// Shared body of the lint renderers. `checked` is `None` when the caller has no
/// run-level count to report (the legacy results-only entry point).
fn render_lint_core(
    results: &[LintResult],
    checked: Option<usize>,
    skipped: &[SkippedFile],
    errors: &[LintError],
    discovery: &DiscoveryReport,
    verbosity: Verbosity,
) -> (String, usize) {
    let mut out = String::new();
    let mut total = 0usize;
    let mut fixable = 0usize;
    for r in results {
        if r.diagnostics.is_empty() {
            if verbosity.debug
                && let Some(debug) = &r.debug
            {
                let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
                render_debug_block(&mut out, debug);
            }
            continue;
        }
        let _ = writeln!(out, "{}", r.path.display().if_supports_color(Stdout, |t| t.bold()));
        for d in &r.diagnostics {
            total += 1;
            if !d.fix.is_empty() {
                fixable += 1;
            }
            let mut segments: Vec<String> = Vec::with_capacity(5);
            segments.push(severity_label(d.severity));
            segments.push(d.engine.if_supports_color(Stdout, |t| t.magenta()).to_string());
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
                    let _ = writeln!(out, "      {}", description.if_supports_color(Stdout, |t| t.dimmed()));
                }
                if let Some(url) = d.url.as_deref() {
                    let _ = writeln!(out, "      {}", url.if_supports_color(Stdout, |t| t.dimmed()));
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
        // "No issues found." on its own cannot distinguish a verified-clean repo
        // from one whose every file was excluded — qualify it with what was
        // looked at and what was pruned.
        let mut tail: Vec<String> = Vec::with_capacity(4);
        if let Some(checked) = checked {
            tail.push(format!("{checked} file(s) linted"));
        }
        if let Some(clause) = skipped_clause(skipped) {
            tail.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            tail.push(clause);
        }
        if let Some(clause) = unrecognized_clause(discovery) {
            tail.push(clause);
        }
        // "No issues found" over an empty file set is a false reassurance
        // whether the files were excluded before the run, skipped during it, or
        // never recognised as source at all — in every case nothing was
        // examined, which is not the same as clean.
        let nothing_linted = checked == Some(0) && (discovery.has_notes() || !skipped.is_empty());
        // A `--fix` run that resolved everything has no diagnostics left to
        // report, so "No issues found." was the summary of a run that had just
        // rewritten files: true about the end state, silent about the act.
        let fixed = fixed_clause(results);
        // A file whose engine errored was accepted for linting and then not
        // linted, so nothing here may read as a verdict on the repo — least of
        // all `Fixed N issue(s)`, which is true about what the run did and silent
        // about what it failed to do. The fix count still gets its own line
        // below, where it cannot stand in for a pass.
        let headline = match (!errors.is_empty(), nothing_linted, &fixed) {
            (true, _, _) => "Lint did not complete."
                .if_supports_color(Stdout, |t| t.red())
                .to_string(),
            (false, true, _) => "Nothing was linted."
                .if_supports_color(Stdout, |t| t.yellow())
                .to_string(),
            (false, false, Some(fixed)) => fixed.if_supports_color(Stdout, |t| t.green()).to_string(),
            (false, false, None) => "No issues found.".if_supports_color(Stdout, |t| t.green()).to_string(),
        };
        if tail.is_empty() {
            let _ = writeln!(out, "{headline}");
        } else {
            let _ = writeln!(out, "{headline} ({})", tail.join(", "));
        }
        if !errors.is_empty()
            && let Some(fixed) = &fixed
        {
            let _ = writeln!(out, "{}", fixed.if_supports_color(Stdout, |t| t.yellow()));
        }
    } else {
        let mut headline = format!("{total} issue(s) found.");
        let mut tail: Vec<String> = Vec::with_capacity(4);
        if let Some(checked) = checked {
            tail.push(format!("{checked} file(s) linted"));
        }
        if let Some(clause) = skipped_clause(skipped) {
            tail.push(clause);
        }
        if let Some(clause) = exclusion_clause(discovery) {
            tail.push(clause);
        }
        if let Some(clause) = unrecognized_clause(discovery) {
            tail.push(clause);
        }
        if !tail.is_empty() {
            let _ = write!(headline, " ({})", tail.join(", "));
        }
        let _ = writeln!(out, "\n{}", headline.if_supports_color(Stdout, |t| t.red()));
        // A partially fixed run reports both halves: what it changed under the
        // caller, and what it left for them. Never green once a file errored —
        // the run is incomplete whatever it managed to fix.
        if let Some(fixed) = fixed_clause(results) {
            let colored = if errors.is_empty() {
                fixed.if_supports_color(Stdout, |t| t.green()).to_string()
            } else {
                fixed.if_supports_color(Stdout, |t| t.yellow()).to_string()
            };
            let _ = writeln!(out, "{colored}");
        }
        if fixable > 0 {
            let _ = writeln!(
                out,
                "{}",
                format!("{fixable} fixable with the `--fix` option.").if_supports_color(Stdout, |t| t.green())
            );
        }
    }
    // What was never discovered is as load-bearing as what was: a finding list
    // is only as trustworthy as the file set behind it.
    push_discovery_note(&mut out, discovery);
    // Same argument one step later in the pipeline: a file that was discovered
    // but never inspected is not evidence of anything, so it is named.
    push_skip_note(&mut out, skipped, verbosity.verbose);
    // Withholding a fix must be visible: an invisible skip is the same failure as
    // an invisible fix, just in the other direction.
    let withheld = results.iter().filter(|r| r.fix_withheld_generated).count();
    if withheld > 0 {
        let _ = writeln!(
            out,
            "{}",
            format!("{withheld} generated file(s) not fixed (pass `--fix-generated` to include them).")
                .if_supports_color(Stdout, |t| t.yellow())
        );
    }
    // Last, next to the exit code it drives: a file the linter could not process
    // was not checked, and the run must name it rather than leave it out.
    out.push_str(&render_lint_errors(errors));
    (out, total)
}

/// Render the files the linter could not process, naming each path.
///
/// A file whose engine errored has not been checked, so it must be visible and it
/// must not be folded into the skipped set — a skip is poly correctly declining a
/// file, an error is poly failing on one. The caller exits 2 on these, the same
/// code `poly fmt` uses for a formatter failure, and distinct from exit 1
/// ("findings remain").
pub fn render_lint_errors(errors: &[LintError]) -> String {
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
        format!("{} file(s) could not be linted and were NOT checked.", errors.len())
            .if_supports_color(Stdout, |t| t.red())
    );
    out
}

/// Print the lint error block to **stderr**, for the `json`/`toon` formats.
///
/// Those formats carry the failures structurally on stdout; this is the
/// human-visible echo, on the stream that cannot corrupt the document — the same
/// split [`eprint_skip_note`](crate::report::eprint_skip_note) uses.
pub fn eprint_lint_errors(errors: &[LintError]) {
    if errors.is_empty() {
        return;
    }
    let plain = strip_ansi(&render_lint_errors(errors));
    eprint!("{}", plain.if_supports_color(Stderr, |t| t.red()));
}

/// Print the human-oriented lint report to stdout. Returns the total
/// diagnostic count.
pub fn report_lint_pretty(results: &[LintResult], verbosity: Verbosity) -> usize {
    let (text, total) = render_lint_pretty(results, verbosity);
    print!("{text}");
    total
}

/// Print the human-oriented lint report for a whole [`LintRun`] to stdout, so
/// the summary is qualified by what was linted and what was excluded. Returns
/// the total diagnostic count.
pub fn report_lint_pretty_run(run: &LintRun, verbosity: Verbosity) -> usize {
    let (text, total) = render_lint_pretty_run(run, verbosity);
    print!("{text}");
    total
}

/// The line reporting what `--fix` rewrote, or `None` when it rewrote nothing.
///
/// `--fix` reports the diagnostics that *remain*, so a run that fixed everything
/// printed `No issues found.` while rewriting files on disk — reporting on what
/// it found instead of on what it did. This states the second half.
fn fixed_clause(results: &[LintResult]) -> Option<String> {
    let issues: usize = results.iter().map(|r| r.fixed).sum();
    if issues == 0 {
        return None;
    }
    let files = results.iter().filter(|r| r.fixed > 0).count();
    Some(format!("Fixed {issues} issue(s) in {files} file(s)."))
}
