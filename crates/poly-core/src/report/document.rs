//! The machine-readable **document**: what a run found, and what it did not
//! look at.
//!
//! `--format json` used to emit a bare array of per-file records. That answered
//! "what did you find" and left "what did you check" to be reconstructed by
//! walking every record and testing two optional fields — which no consumer
//! did, so a run that skipped everything was indistinguishable from a clean one.
//! Errors were promoted to a top-level list for exactly that reason; this does
//! the same for skips, and adds the count that makes the question answerable in
//! one comparison instead of a scan.
//!
//! The array shape moved to an object to do it. That is a breaking change to
//! `poly lint --format json`, taken deliberately and in one step for both the
//! CLI and the MCP server, so the two cannot answer the same question
//! differently.

use serde::Serialize;

use crate::ConfigFingerprint;
use crate::runner::{FormatError, FormatResult, FormatRun, LintError, LintResult, LintRun, SkippedFile};

/// What a run actually did, in three numbers.
///
/// **These cannot be derived from `results`, which is why the run states them.**
/// `results` holds only files with something to report, so a file that was
/// checked and found clean appears in no record at all — `checked` counts files
/// that are simply absent from the list. In the other direction a file can be
/// both a result and a skip, since the cross-cutting backends (spell-check,
/// ast-grep, the quality tier) still run over a file whose *language* nothing
/// holds lint rules for, so it can carry findings while not counting as linted.
/// A consumer asking "was everything checked" should compare
/// [`checked`](Self::checked) against the file count it expected.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunSummary {
    /// Files the run inspected and had rules for.
    ///
    /// Excludes files whose language nothing in the run lints, which are in
    /// [`skipped`](LintDocument::skipped) with the reason attached.
    pub checked: usize,
    /// Files nothing inspected — the length of the `skipped` list.
    pub skipped: usize,
    /// Files the run **failed** on — the length of the `errors` list. These were
    /// not checked, whatever else the document says.
    pub errored: usize,
}

/// A whole lint run, as a machine consumer sees it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LintDocument {
    /// Per-file results, including synthetic entries for files the run failed
    /// on or declined, which carry no diagnostics of their own.
    pub results: Vec<LintResult>,
    /// Files the run **failed** on. Redundant with the `error`-carrying entries
    /// in `results` on purpose: the defect this closes is a consumer reading a
    /// clean-looking list and concluding the files are fine.
    pub errors: Vec<LintError>,
    /// Files nothing inspected, each with the reason. Redundant with the
    /// `skipped`-carrying entries in `results` for the same reason `errors` is.
    pub skipped: Vec<SkippedFile>,
    /// The run's own account of what it covered.
    pub summary: RunSummary,
    /// The configurations that governed this run, indexed by each result's
    /// `config` field.
    ///
    /// Two runs of an identical binary can enforce different rules — a
    /// `poly.toml`, a `poly.local.toml`, a nested config or an `extends` base
    /// can move underneath it — and both report clean. This is what tells a
    /// consumer whether two clean reports are comparable at all. A monorepo run
    /// legitimately carries several entries, each naming the directory it
    /// resolved from, so a difference between sibling packages is attributable
    /// rather than anomalous.
    pub configs: Vec<ConfigFingerprint>,
}

impl LintDocument {
    /// Build the document from a whole run.
    pub fn from_run(run: &LintRun) -> Self {
        Self {
            results: super::structured::lint_results_for_output(run),
            errors: run.errors.clone(),
            skipped: run.skipped.clone(),
            summary: RunSummary {
                checked: run.checked,
                skipped: run.skipped.len(),
                errored: run.errors.len(),
            },
            configs: run.configs.clone(),
        }
    }
}

/// A whole format run, as a machine consumer sees it. The format counterpart of
/// [`LintDocument`], field for field and for the same reasons.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FormatDocument {
    /// Per-file results, including synthetic entries for failed and declined
    /// files.
    pub results: Vec<FormatResult>,
    /// Files the run failed on.
    pub errors: Vec<FormatError>,
    /// Files nothing inspected, each with the reason.
    pub skipped: Vec<SkippedFile>,
    /// The run's own account of what it covered.
    pub summary: RunSummary,
    /// The configurations that governed this run, indexed by each result's
    /// `config` field.
    ///
    /// Two runs of an identical binary can enforce different rules — a
    /// `poly.toml`, a `poly.local.toml`, a nested config or an `extends` base
    /// can move underneath it — and both report clean. This is what tells a
    /// consumer whether two clean reports are comparable at all. A monorepo run
    /// legitimately carries several entries, each naming the directory it
    /// resolved from, so a difference between sibling packages is attributable
    /// rather than anomalous.
    pub configs: Vec<ConfigFingerprint>,
}

impl FormatDocument {
    /// Build the document from a whole run.
    pub fn from_run(run: &FormatRun) -> Self {
        Self {
            results: super::structured::format_results_for_output(run),
            errors: run.errors.clone(),
            skipped: run.skipped.clone(),
            summary: RunSummary {
                checked: run.checked,
                skipped: run.skipped.len(),
                errored: run.errors.len(),
            },
            configs: run.configs.clone(),
        }
    }
}
