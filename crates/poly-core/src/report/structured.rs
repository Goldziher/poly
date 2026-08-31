//! The machine-readable renderers: `json` (`serde_json`) and `toon`
//! (Token-Oriented Object Notation).
//!
//! The `*_run` variants render a whole [`LintDocument`] / [`FormatDocument`] —
//! results, errors, skips and the coverage summary. The bare-slice variants
//! render just the per-file records, and exist for callers that already hold a
//! result set rather than a run.

use super::RenderError;
use super::document::{FormatDocument, LintDocument};
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
/// different outcomes — with an empty `diagnostics` list, so every file the run
/// touched has a record whether or not it produced findings.
pub(super) fn lint_results_for_output(run: &LintRun) -> Vec<LintResult> {
    let mut results = run.results.clone();
    let mut known: std::collections::BTreeSet<&std::path::Path> =
        run.results.iter().map(|r| r.path.as_path()).collect();
    let synthetic = |path: &std::path::Path, skipped: Option<String>, error: Option<String>| LintResult {
        suppressed: Vec::new(),
        path: path.to_path_buf(),
        // A run-level error or skip carries no per-file routing, so it is
        // attributed to the run's root config rather than guessed at.
        config: 0,
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

/// A whole [`LintRun`] as JSON: results, errors, skips and the coverage summary,
/// so the skipped set and the checked count are answerable without walking every
/// record.
pub fn report_lint_json_run(run: &LintRun) -> Result<String, RenderError> {
    render_json(&LintDocument::from_run(run))
}

/// [`report_lint_json_run`] as TOON.
pub fn report_lint_toon_run(run: &LintRun) -> Result<String, RenderError> {
    render_toon(&LintDocument::from_run(run))
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
pub(super) fn format_results_for_output(run: &FormatRun) -> Vec<FormatResult> {
    let mut results = run.results.clone();
    let mut known: std::collections::BTreeSet<&std::path::Path> =
        run.results.iter().map(|r| r.path.as_path()).collect();
    let synthetic = |path: &std::path::Path, skipped: Option<String>, error: Option<String>| FormatResult {
        path: path.to_path_buf(),
        // As on the lint side: no per-file routing, so the run's root config.
        config: 0,
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

/// A whole [`FormatRun`] as JSON — the format counterpart of
/// [`report_lint_json_run`].
pub fn report_format_json_run(run: &FormatRun) -> Result<String, RenderError> {
    render_json(&FormatDocument::from_run(run))
}

/// [`report_format_json_run`] as TOON.
pub fn report_format_toon_run(run: &FormatRun) -> Result<String, RenderError> {
    render_toon(&FormatDocument::from_run(run))
}
