//! Whether the run acts on machine-generated files, and the record it leaves
//! behind when it does not.
//!
//! Two different questions live in this area and were, for a while, answered by
//! two different predicates in two different phases. They are separated here so
//! neither drifts again:
//!
//! 1. **Does poly check this file at all?** `[discovery] generated`
//!    ([`acts_on_generated`]) — a user preference, spanning `lint`, `fmt` and
//!    `--fix` alike, defaulting to *yes*. Answered from
//!    [`crate::filter::is_generated_source`], the broad marker scan.
//! 2. **May poly rewrite a file it did check?** Answered in
//!    [`super::format_one`] and [`super::lint_one`] from
//!    [`crate::filter::is_hash_stamped_source`], the narrow one — see
//!    [`super::HASH_STAMPED_SKIP`].
//!
//! The second is not a noise preference and is not covered by the first: a
//! banner announces provenance, while a content-hash stamp is a claim about the
//! bytes, and reformatting those bytes puts the generator's own verify step into
//! a regen loop. `lint --fix` and `fmt` now ask that question identically; it
//! was `lint --fix` withholding on *any* generated marker that made a
//! banner-only file reformattable by `poly fmt` and unfixable by `poly lint
//! --fix` in the same repository.

use crate::discover::DiscoveredFile;
use crate::resolve::ConfigSet;

use super::types::{FormatResult, LintResult};

/// Resolve `[discovery] generated` once per config in the run.
///
/// Returned as a `Vec<bool>` indexed by [`DiscoveredFile::config_id`] so the
/// per-file body inside the rayon `par_iter` costs one indexed load — and, on
/// the default `true`, short-circuits before the content scan runs at all. The
/// scan is cheap; cheap per file times a repository is not, and nothing about
/// the default path needs it.
///
/// `run_override` is the caller's per-run flag (`--skip-generated` /
/// `--include-generated`), which beats every config in the set: a one-off
/// override that a nested `poly.toml` could veto would not be an override.
/// `None` defers to each config, so a nested config governs its own subtree
/// (ADR 0018).
pub(crate) fn acts_on_generated(configs: &ConfigSet, run_override: Option<bool>) -> Vec<bool> {
    configs
        .iter()
        .map(|config| run_override.unwrap_or(config.generated))
        .collect()
}

/// The lint record for a file the run never inspected, carrying `reason` — the
/// generated opt-out or a binary artifact.
///
/// The reason is a parameter rather than a constant because the two skips are
/// indistinguishable in structure and must stay distinguishable in report: one
/// says the reader opted out in their own `poly.toml`, the other says the bytes
/// were never text. Collapsing them to one wording would tell a reader looking
/// at 1,263 skipped catalogs to go edit a key they never set.
pub(crate) fn lint_skip_result(file: &DiscoveredFile, reason: &str) -> LintResult {
    LintResult {
        path: file.path.clone(),
        config: file.config_id,
        diagnostics: Vec::new(),
        // Nothing was withheld: the file was never linted, so there was no fix
        // to hold back. The skip reason is the whole story here.
        fix_withheld_generated: false,
        fixed: 0,
        skipped: Some(reason.to_owned()),
        error: None,
        debug: None,
    }
}

/// The format record for a file no engine ran on, carrying `reason` — the
/// generated opt-out, the hash stamp, or a backend declining the file.
///
/// `changed: false` is the load-bearing part: a skipped file must not be
/// reported as drift under `--check`, or the gate can never go green on a file
/// poly has decided not to write.
pub(crate) fn format_skip_result(file: &DiscoveredFile, reason: Option<String>) -> FormatResult {
    FormatResult {
        path: file.path.clone(),
        config: file.config_id,
        changed: false,
        formatted: None,
        skipped: reason,
        error: None,
        // No engine ran, so there is nothing to time: the debug block reports
        // engines that executed.
        debug: None,
    }
}
