//! The run's public data model: the options a caller passes in, and the
//! per-file and run-level records that come back out.
//!
//! Split from [`super`] so the pipeline file holds the pipeline. These types
//! are the API surface every front end (`poly lint`, the MCP server, a library
//! caller) serializes and asserts against, and they change for different
//! reasons than the code that fills them in.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::discover::DiscoveryReport;
use crate::engine::Diagnostic;
use crate::language::Language;
use crate::runner::SkippedFile;

/// Options controlling a lint/format run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Bypass the content-hash result cache.
    pub no_cache: bool,
    /// Number of worker threads; `None` => all logical cores.
    pub jobs: Option<usize>,
    /// Extra gitignore-style exclude globs supplied at call time (CLI `--exclude`
    /// / MCP `exclude`), merged with the config's `[discovery] exclude`.
    pub exclude: Vec<String>,
    /// Apply the exclude set to explicitly named roots as well as to the walk.
    ///
    /// A hook is always handed explicit staged paths, so without this the
    /// repo's `[discovery] exclude` is silently inert exactly where it matters
    /// most. The CLI, hooks, and MCP turn this on by default.
    pub force_exclude: bool,
    /// Apply `--fix` to machine-generated files too. Off by default: a fix there
    /// is reverted by the next generation run, and can silence the diagnostic
    /// that was the only evidence of a generator bug.
    pub fix_generated: bool,
    /// When `true`, the caller supplied an explicit `--config <path>`: use that
    /// single config for every file and skip hierarchical (nested `poly.toml`)
    /// resolution (ADR 0018). Default `false` — scan for nested configs.
    pub explicit_config: bool,
    /// Resolver for `extends` bases (ADR 0020). When set, nested `poly.toml`
    /// files resolve their `extends` list (local or pinned remote git bases)
    /// through this resolver during the cascade. `None` => local-only resolution
    /// via `LocalPathResolver` (the default; no remote fetch).
    pub config_resolver: Option<Arc<dyn poly_config::BaseConfigResolver>>,
    /// Languages that **another phase of the same run** lints, outside the
    /// per-file tier.
    ///
    /// `poly lint` runs a whole-project phase (`cargo clippy` and any configured
    /// whole-project analysis job) that the per-file tier knows nothing about.
    /// Without this, a run that had just linted every `.rs` file with clippy also
    /// printed `no lint rules for Rust` against each of them — a statement its
    /// own output contradicted three lines later. The caller that orchestrates
    /// both phases is the only layer that can see both, so it declares the
    /// overlap here and the per-file tier stops calling those files skipped at
    /// all: the count, the note, the JSON payload and `--deny-skips` then agree
    /// by construction rather than by a display-time filter.
    ///
    /// Empty by default, so a library caller running only the per-file tier —
    /// and `poly lint --no-workspace`, where nothing else does lint Rust — keeps
    /// the accurate skip.
    pub externally_linted_languages: Vec<Language>,
}

/// Per-engine debug record for one file. Collected only when debug output is
/// requested (`--debug`); never built on the default hot path.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EngineDebug {
    /// Backend that produced this record.
    pub engine: String,
    /// Wrapped tool/crate version (matches the cache-key component).
    pub version: String,
    /// Wall-clock time the engine spent on this file, in milliseconds. Zero for
    /// a cache hit (the engine did not run).
    pub duration_ms: f64,
    /// Whether the result came from the content-hash cache.
    pub cache_hit: bool,
}

/// Per-file debug data surfaced under `--debug`: cache hit/miss and timing for
/// each engine that ran. Populated only when debug collection is enabled.
#[derive(Debug, Clone, Serialize, Default)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RunDebug {
    /// One entry per engine evaluated for the file.
    pub engines: Vec<EngineDebug>,
}

/// Serde predicate: omit a zero count so a run that fixed nothing keeps exactly
/// the JSON shape consumers already parse.
fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// Per-file lint outcome.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LintResult {
    /// File that was linted.
    pub path: PathBuf,
    /// Diagnostics from all backends for this file.
    pub diagnostics: Vec<Diagnostic>,
    /// Set when `--fix` was requested but withheld because the file announces
    /// itself as machine-generated. The diagnostics are still reported.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fix_withheld_generated: bool,
    /// How many diagnostics `--fix` resolved by rewriting this file.
    ///
    /// A `--fix` run reports the diagnostics that *remain*, which for a fully
    /// fixed file is none — so the run printed `No issues found.` while it was
    /// rewriting files on disk, and a consumer whose autofix destroyed content
    /// had nothing in the output pointing at the run that did it. This is the
    /// count of what the run *did*, kept alongside what it found.
    #[serde(skip_serializing_if = "is_zero")]
    pub fixed: usize,
    /// Why no lint rules were applied to this file, when none were.
    ///
    /// Set by the runner when the file's language has no backend carrying rules
    /// for it (`no lint rules for Kotlin`), and by the JSON/TOON renderers on
    /// the synthetic entries they append for paths no engine covers at all (see
    /// [`crate::report::report_lint_json_run`]) — that is what lets a consumer
    /// assert on the skipped *set* structurally instead of scraping the human
    /// summary.
    ///
    /// `diagnostics` may still be non-empty alongside it: the cross-cutting
    /// backends (spell-check, comment removal, ast-grep) run on every routed
    /// file, so a Kotlin file can carry a typo finding while remaining
    /// uncovered by any Kotlin rule. Reporting one does not retract the other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// The engine failure that stopped this file being linted, when one did.
    ///
    /// Deliberately a separate field from [`LintResult::skipped`]: a skip is poly
    /// correctly declining a file, an error is poly failing on one it accepted,
    /// and a consumer that cannot tell them apart is back to trusting a run that
    /// checked less than it claimed. Populated on the synthetic entries the
    /// JSON/TOON renderers append for [`LintRun::errors`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
}

/// Per-file format outcome.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FormatResult {
    /// File that was formatted.
    pub path: PathBuf,
    /// Whether formatting changed (or would change) the file.
    pub changed: bool,
    /// Why no backend inspected this file, when none did.
    ///
    /// A file routed to a backend that declines it — YAML carrying Go/Helm
    /// template actions, a Jinja template rendering Go — was previously
    /// indistinguishable in the report from one that was checked and found
    /// clean. Carrying the reason lets the summary say so.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    /// The engine failure that stopped this file being formatted, when one did.
    ///
    /// Deliberately a separate field from [`FormatResult::skipped`], for the same
    /// reason [`LintResult::error`] is: a skip is poly correctly declining a file,
    /// an error is poly failing on one it accepted. Without it the format JSON
    /// could not express an errored file at all, so `poly fmt --check --format
    /// json` omitted it entirely and a file poly had failed to read was
    /// indistinguishable from one checked and found clean. Populated on the
    /// synthetic entries the JSON/TOON renderers append for [`FormatRun::errors`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Formatted contents when changed (not serialized).
    #[serde(skip)]
    pub formatted: Option<String>,
    /// Debug data (cache hit/miss + timing), present only under `--debug`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<RunDebug>,
}

/// A complete lint run: the per-file results plus the run-level accounting a
/// summary needs to qualify itself.
///
/// [`super::lint`] returns only the results, which cannot express "I checked nothing,
/// because everything was excluded" — the failure mode this type exists to fix.
#[derive(Debug, Clone)]
pub struct LintRun {
    /// Per-file results, one per file that still has at least one diagnostic.
    pub results: Vec<LintResult>,
    /// Files whose lint *failed* — an unreadable file, a backend that returned
    /// an error, a bad engine config.
    ///
    /// These used to be logged at `warn` and dropped from the results, so a file
    /// the run had failed to process simply vanished from it and `poly lint`
    /// reported `No issues found.` and exited 0. A backend that errored has not
    /// checked the file, and saying otherwise is a gate that passes without
    /// checking — the same defect [`FormatRun::errors`] exists to prevent.
    ///
    /// Kept apart from [`LintRun::skipped`] on purpose: a skip is poly declining
    /// a file it does not handle, an error is poly failing on a file it took on.
    pub errors: Vec<LintError>,
    /// Files the per-file tier read and applied lint rules to.
    ///
    /// Excludes files whose language nothing in the run has rules for. Those are
    /// read, and the cross-cutting backends do run over them, but counting them
    /// here made `N file(s) linted` include languages poly cannot lint — they
    /// are in [`LintRun::skipped`] instead, with the language named.
    pub checked: usize,
    /// Files the run did not lint, each with its reason — files whose language
    /// no backend has rules for, explicitly named paths no engine covers, and
    /// files every routed backend declined.
    ///
    /// A count alone forced consumers to reconstruct the set from a heuristic
    /// and parse it back out of the human summary, so the names travel with it.
    pub skipped: Vec<SkippedFile>,
    /// What `[discovery] exclude` / `--exclude` pruned before any of that.
    pub discovery: DiscoveryReport,
}

/// A complete format run: the per-file results plus what discovery pruned.
#[derive(Debug, Clone)]
pub struct FormatRun {
    /// Per-file results, one per discovered file.
    pub results: Vec<FormatResult>,
    /// Files whose formatter *errored* — a syntax error the engine could not
    /// parse, an unreadable file, a bad engine config.
    ///
    /// These used to be logged at `warn` and dropped from the results, so the
    /// run reported `All formatted.` and exited 0 on a file it had failed to
    /// process. A formatter that cannot parse a file has not verified it, and
    /// saying otherwise is a gate that passes without checking. Carried here so
    /// the caller can name the paths and fail.
    pub errors: Vec<FormatError>,
    /// Files the run did not inspect, each with its reason — files every routed
    /// backend declined, plus explicitly named paths no engine covers.
    ///
    /// The per-file [`FormatResult::skipped`] already carries the former; this
    /// is the run-level union, so one strict-mode check covers both kinds.
    pub skipped: Vec<SkippedFile>,
    /// What `[discovery] exclude` / `--exclude` pruned before any of that.
    pub discovery: DiscoveryReport,
}

/// One file the formatter could not process, and why.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FormatError {
    /// File that could not be formatted.
    pub path: PathBuf,
    /// The engine's error, already flattened to a message.
    pub message: String,
}

/// One file the linter could not process, and why.
///
/// The lint counterpart of [`FormatError`], kept as its own type so each side
/// documents the failure in its own terms and neither is silently reshaped by a
/// change to the other.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct LintError {
    /// File that could not be linted.
    pub path: PathBuf,
    /// The engine's error, already flattened to a message.
    pub message: String,
}
