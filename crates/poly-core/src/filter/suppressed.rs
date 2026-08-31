//! What a run **dropped**, and which mechanism dropped it.
//!
//! poly's reporting surface follows one rule: *a limitation of poly is charged
//! to the coverage budget and names the limitation; an instruction from the
//! caller is never charged, and names itself.* Skips had the second half —
//! every skip carries a reason — and suppressions did not. Three mechanisms
//! removed diagnostics from a report with nothing anywhere saying so: a pack
//! rule's default path exclusions, a `[per-file-ignores]` glob match, and an
//! in-source `poly: allow[…]` directive.
//!
//! A [`SuppressedDiagnostic`] per dropped finding closes that. It is not a
//! coverage claim — every one of these is something the caller asked for, so
//! none is charged to `--deny-skips` — but it makes the ask auditable: the
//! unfiltered finding set is `results` plus `suppressed`, reconstructible from
//! a single run rather than by re-running poly with the filters switched off.

use std::path::PathBuf;

use serde::Serialize;

/// Which mechanism dropped a diagnostic.
///
/// Kept as distinct variants rather than a free-text reason because the three
/// are fixed in a different place: a default path exclusion is edited in the
/// rule's own YAML (or overridden in `[per-file-ignores]`), a per-file ignore
/// in `poly.toml`, and an inline suppression in the source file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum SuppressionReason {
    /// A rule's own `ignores:` globs, declared in the rule YAML that defines
    /// it — poly's built-in ast-grep pack ships several. These are *defaults*:
    /// naming the rule in `[per-file-ignores]` replaces them outright.
    DefaultPathExclusion,
    /// A `[per-file-ignores]` entry in the resolved `poly.toml`.
    PerFileIgnore,
    /// An in-source `poly: allow[…]` / `poly: allow-file[…]` directive
    /// (ADR 0028).
    InlineSuppression,
}

/// One diagnostic a run found and then dropped, and why.
///
/// The `code` is optional because a code-less diagnostic is suppressible too —
/// `poly: allow-file[*]` covers everything, including findings no engine gave
/// a rule id. Omitting the field rather than inventing a placeholder keeps the
/// entry honest: it still counts toward `results + suppressed`, and a reader
/// can see that poly had no rule name to report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SuppressedDiagnostic {
    /// File the dropped diagnostic was reported against.
    pub path: PathBuf,
    /// The diagnostic's rule code, when it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The mechanism that dropped it.
    pub reason: SuppressionReason,
}

impl SuppressedDiagnostic {
    /// Record `diagnostic` as dropped from `path` by `reason`.
    pub(crate) fn new(
        path: &std::path::Path,
        diagnostic: &crate::engine::Diagnostic,
        reason: SuppressionReason,
    ) -> Self {
        Self {
            path: path.to_path_buf(),
            code: diagnostic.code.clone(),
            reason,
        }
    }
}
