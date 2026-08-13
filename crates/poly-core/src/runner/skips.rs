//! Accounting for files a run did **not** inspect.
//!
//! Three distinct things end up here, because a consumer cannot tell them apart
//! from a summary that reports none of them:
//!
//! - a backend routed the file and then declined it (Go-templated YAML, a
//!   hash-stamped generated file) — surfaced by [`crate::engine::Engine::skip_reason`];
//! - a path named **explicitly** on the command line that no engine covers at
//!   all. `poly lint packages/csharp/App.csproj` reported `No issues found. (0
//!   file(s) linted)` and exited 0: nothing was examined, and nothing said so.
//!   The mixed case is the dangerous one — five explicit paths in, four linted
//!   out, with no indication of which was dropped.
//! - a file whose language **no backend in the run holds lint rules for**
//!   ([`SkippedFile::no_lint_rules`]). This one *was* routed — the cross-cutting
//!   backends run over it and can still report findings — so it looked exactly
//!   like a checked file in the summary. `poly lint .` over a Kotlin/Swift/Zig
//!   repo counted every file as linted and exited 0 with nothing holding a
//!   Kotlin, Swift or Zig rule.
//!
//! Only *explicit* paths are accounted for. A directory walk legitimately
//! contains files no engine handles; narrating those would make every run noisy
//! and is not what the caller asked about. Naming a path, by contrast, is a
//! request to check that path.
//!
//! Split out of `runner.rs` so the runner keeps to the pipeline itself.

use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;
use serde::Serialize;

use super::plan::PlanMap;
use crate::discover::{DiscoveredFile, discover_with};
use crate::language::Language;
use crate::resolve::ConfigSet;

/// Reason recorded for a path the caller named that no backend covers.
///
/// Kept as one constant so the human summary, the JSON payload, and the
/// `--deny-skips` failure all quote the same words.
pub const NO_ENGINE_SKIP: &str = "no matching engine for this file type";

/// Opening words of the reason recorded for a file whose language nothing in the
/// run has lint rules for; completed with the language name.
///
/// Deliberately not [`NO_ENGINE_SKIP`]: "no matching engine for this file type"
/// is false here — an engine *did* match, poly simply has no rules for the
/// language, which is a different thing to act on. One says "poly does not know
/// this file type"; the other says "poly knows this language and has nothing to
/// say about it", and the fix for the second is a linter for that language
/// (`[tools.<name>]`), not a rename or an exclude.
pub const NO_LINT_RULES_SKIP_PREFIX: &str = "no lint rules for";

/// One file the run did not inspect, and why.
///
/// The reason is what makes the entry actionable: a bare list of paths tells a
/// consumer *that* poly declined without telling them whether to fix the file,
/// the config, or their expectations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SkippedFile {
    /// File that was not inspected.
    pub path: PathBuf,
    /// Why no backend looked at it.
    pub reason: String,
}

impl SkippedFile {
    /// A skip for a path the caller named that no engine covers.
    pub(super) fn no_engine(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            reason: NO_ENGINE_SKIP.to_owned(),
        }
    }

    /// The reason text for a file whose language nothing in the run lints,
    /// naming the language so the reader knows what is missing rather than only
    /// that something is.
    pub(super) fn no_lint_rules_reason(language: &Language) -> String {
        format!("{NO_LINT_RULES_SKIP_PREFIX} {}", language.display_name())
    }
}

/// Number of explicitly named file arguments from which reconciling them against
/// the discovered set is worth an index rather than a scan.
///
/// Building the index touches every byte of every discovered path (hashing it)
/// and allocates a table; a scan compares paths and stops at the first match. So
/// the index only pays off once several arguments share its cost — which is also
/// the point at which the scans stop being a rounding error next to the run they
/// belong to.
const EXPLICIT_PATHS_INDEX_THRESHOLD: usize = 8;

/// The explicitly named file arguments that no engine will look at.
///
/// A path qualifies when it is a file (directories are walked, and what a walk
/// does not match is not a skip) and either discovery never identified a
/// language for it, or the language it identified has an empty engine plan.
///
/// Under `--force-exclude` an absent path is ambiguous: the exclude set may have
/// dropped it, which the discovery note already reports. The two are told apart
/// by re-walking that single path with excludes off — an O(1) walk of one file,
/// paid only for a path that is already known to be missing from the run.
pub(super) fn unmatched_explicit_paths(
    paths: &[PathBuf],
    files: &[DiscoveredFile],
    plans: &PlanMap,
    configs: &ConfigSet,
    exclude: &[String],
    force_exclude: bool,
) -> Vec<PathBuf> {
    // The overwhelmingly common invocation is `poly lint .` — no file arguments
    // at all, so nothing to reconcile at all. One `is_file` stat per argument,
    // never per discovered file.
    let explicit: Vec<&PathBuf> = paths.iter().filter(|path| path.is_file()).collect();
    if explicit.is_empty() {
        return Vec::new();
    }
    // A mixed invocation (`poly lint src/foo.py .`) does reach here with a full
    // corpus behind it, so the lookup is only indexed once enough arguments
    // amortise building the index — below that a scan of `files` is cheaper than
    // hashing every discovered path into a table used a handful of times.
    let index: Option<FxHashMap<&Path, &DiscoveredFile>> = (explicit.len() >= EXPLICIT_PATHS_INDEX_THRESHOLD)
        .then(|| files.iter().map(|f| (f.path.as_path(), f)).collect());
    let mut unmatched = Vec::new();
    for path in explicit {
        let discovered = match &index {
            Some(index) => index.get(path.as_path()).copied(),
            None => files.iter().find(|file| file.path == **path),
        };
        match discovered {
            Some(file) => {
                let routed = plans
                    .get(&(file.config_id, file.language.clone()))
                    .is_some_and(|plans| !plans.is_empty());
                if !routed {
                    unmatched.push(path.clone());
                }
            }
            None if excluded_rather_than_unmatched(path, configs, exclude, force_exclude) => {}
            None => unmatched.push(path.clone()),
        }
    }
    unmatched
}

/// Whether an explicitly named path is missing from the run because the exclude
/// set dropped it (already reported by the discovery note) rather than because
/// no engine covers it.
fn excluded_rather_than_unmatched(path: &Path, configs: &ConfigSet, exclude: &[String], force_exclude: bool) -> bool {
    // Without `--force-exclude` an explicitly named file is never dropped by the
    // exclude set, so absence can only mean "unidentified".
    if !force_exclude {
        return false;
    }
    let single = [path.to_path_buf()];
    !discover_with(&single, configs, exclude, false).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_engine_skip_carries_the_path_and_the_shared_reason() {
        let skipped = SkippedFile::no_engine(Path::new("App.csproj"));
        assert_eq!(skipped.path, PathBuf::from("App.csproj"));
        assert_eq!(skipped.reason, NO_ENGINE_SKIP);
    }

    /// The reason names the language, and names it the way a person writes it —
    /// `no lint rules for Kotlin` is something a reader can act on, where a bare
    /// `skipped` sends them to the source to find out what happened.
    #[test]
    fn no_lint_rules_reason_names_the_language() {
        assert_eq!(
            SkippedFile::no_lint_rules_reason(&Language::Kotlin),
            "no lint rules for Kotlin"
        );
        assert_eq!(
            SkippedFile::no_lint_rules_reason(&Language::Other("elixir".to_owned())),
            "no lint rules for elixir",
            "a tier-2 language is known only by its grammar id"
        );
    }

    /// The two reasons must stay distinguishable: "no matching engine for this
    /// file type" would be false for a routed language, and a consumer grepping
    /// for one must not silently catch the other.
    #[test]
    fn the_two_uninspected_reasons_are_not_the_same_text() {
        assert!(!SkippedFile::no_lint_rules_reason(&Language::Kotlin).contains(NO_ENGINE_SKIP));
        assert!(!NO_ENGINE_SKIP.contains(NO_LINT_RULES_SKIP_PREFIX));
    }
}
