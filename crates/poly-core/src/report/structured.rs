//! The machine-readable renderers: `json` (`serde_json`) and `toon`
//! (Token-Oriented Object Notation), plus the run-level variants that append the
//! skipped and errored files so the document answers "what did you not look
//! at?".

use super::RenderError;
use super::render::{render_json, render_toon};
use crate::runner::{FormatResult, FormatRun, LintResult, LintRun};

/// Render lint results as pretty-printed JSON. The full structured record is
/// always emitted; serde omits `None`/empty fields. The `debug` field is present
/// only when the run collected it (`--debug`).
pub fn report_lint_json(results: &[LintResult]) -> Result<String, RenderError> {
    render_json(results)
}

/// Render lint results as TOON. Falls back to JSON if TOON serialization fails
/// so output is never silently dropped.
pub fn report_lint_toon(results: &[LintResult]) -> Result<String, RenderError> {
    render_toon(results)
}

/// The lint results of a run, with one entry appended per file the run skipped
/// and per file whose engine failed.
///
/// Neither produces diagnostics, so both are filtered out of
/// [`LintRun::results`] and used to be invisible to a machine consumer — which
/// left `--format json` unable to answer "what did you not look at?" and forced
/// one team to reconstruct the set from a heuristic and scrape the human
/// summary for it. The appended entries carry `path` plus `skipped` *or*
/// `error` — never both, since a file poly declined and a file poly failed on are
/// different outcomes — with an empty `diagnostics` list, so the document stays
/// the same array of per-file records and existing consumers are unaffected.
fn lint_results_for_output(run: &LintRun) -> Vec<LintResult> {
    let mut results = run.results.clone();
    let mut known: std::collections::BTreeSet<&std::path::Path> =
        run.results.iter().map(|r| r.path.as_path()).collect();
    let synthetic = |path: &std::path::Path, skipped: Option<String>, error: Option<String>| LintResult {
        path: path.to_path_buf(),
        diagnostics: Vec::new(),
        fix_withheld_generated: false,
        fixed: 0,
        skipped,
        error,
        debug: None,
    };
    // Errors first: a file that failed is reported as failed even if some other
    // stage also listed it, never downgraded to a skip.
    for error in &run.errors {
        if known.insert(error.path.as_path()) {
            results.push(synthetic(&error.path, None, Some(error.message.clone())));
        }
    }
    for entry in &run.skipped {
        if known.insert(entry.path.as_path()) {
            results.push(synthetic(&entry.path, Some(entry.reason.clone()), None));
        }
    }
    results
}

/// [`report_lint_json`] over a whole [`LintRun`], so the skipped and errored sets
/// are carried structurally rather than left to the human summary.
pub fn report_lint_json_run(run: &LintRun) -> Result<String, RenderError> {
    report_lint_json(&lint_results_for_output(run))
}

/// [`report_lint_toon`] over a whole [`LintRun`], including the skipped and
/// errored sets.
pub fn report_lint_toon_run(run: &LintRun) -> Result<String, RenderError> {
    report_lint_toon(&lint_results_for_output(run))
}

/// Render format results as pretty-printed JSON.
pub fn report_format_json(results: &[FormatResult]) -> Result<String, RenderError> {
    render_json(results)
}

/// Render format results as TOON. Falls back to JSON if TOON serialization
/// fails so output is never silently dropped.
pub fn report_format_toon(results: &[FormatResult]) -> Result<String, RenderError> {
    render_toon(results)
}

/// The format results of a run, with one entry appended per file the run failed
/// on and per skipped path that has no result of its own.
///
/// A file a backend declined already appears in [`FormatRun::results`] carrying
/// its `skipped` reason; a path named on the command line that no engine covers
/// never becomes a result at all, and neither does a file the formatter *failed*
/// on — that one used to be dropped from the document entirely, leaving it
/// indistinguishable from a file that was checked and found clean, with the
/// failure visible only in the exit code. Both are added here, so the JSON answer
/// to "what did you not look at?" is complete and matches the lint side's
/// (see [`lint_results_for_output`]).
fn format_results_for_output(run: &FormatRun) -> Vec<FormatResult> {
    let mut results = run.results.clone();
    let mut known: std::collections::BTreeSet<&std::path::Path> =
        run.results.iter().map(|r| r.path.as_path()).collect();
    let synthetic = |path: &std::path::Path, skipped: Option<String>, error: Option<String>| FormatResult {
        path: path.to_path_buf(),
        changed: false,
        skipped,
        error,
        formatted: None,
        debug: None,
    };
    // Errors first: a file that failed is reported as failed even if some other
    // stage also listed it, never downgraded to a skip.
    for error in &run.errors {
        if known.insert(error.path.as_path()) {
            results.push(synthetic(&error.path, None, Some(error.message.clone())));
        }
    }
    for entry in &run.skipped {
        if known.insert(entry.path.as_path()) {
            results.push(synthetic(&entry.path, Some(entry.reason.clone()), None));
        }
    }
    results
}

/// [`report_format_json`] over a whole [`FormatRun`], so every errored and
/// skipped path is carried structurally.
pub fn report_format_json_run(run: &FormatRun) -> Result<String, RenderError> {
    report_format_json(&format_results_for_output(run))
}

/// [`report_format_toon`] over a whole [`FormatRun`], including every errored and
/// skipped path.
pub fn report_format_toon_run(run: &FormatRun) -> Result<String, RenderError> {
    report_format_toon(&format_results_for_output(run))
}
