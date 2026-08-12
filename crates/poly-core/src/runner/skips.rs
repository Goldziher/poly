//! Accounting for files a run did **not** inspect.
//!
//! Two distinct things end up here, because a consumer cannot tell them apart
//! from a summary that reports neither:
//!
//! - a backend routed the file and then declined it (Go-templated YAML, a
//!   hash-stamped generated file) — surfaced by [`crate::engine::Engine::skip_reason`];
//! - a path named **explicitly** on the command line that no engine covers at
//!   all. `poly lint packages/csharp/App.csproj` reported `No issues found. (0
//!   file(s) linted)` and exited 0: nothing was examined, and nothing said so.
//!   The mixed case is the dangerous one — five explicit paths in, four linted
//!   out, with no indication of which was dropped.
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
use crate::resolve::ConfigSet;

/// Reason recorded for a path the caller named that no backend covers.
///
/// Kept as one constant so the human summary, the JSON payload, and the
/// `--deny-skips` failure all quote the same words.
pub const NO_ENGINE_SKIP: &str = "no matching engine for this file type";

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
}

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
    // at all, so nothing to reconcile and no index worth building. One `is_file`
    // stat per argument, never per discovered file.
    let explicit: Vec<&PathBuf> = paths.iter().filter(|path| path.is_file()).collect();
    if explicit.is_empty() {
        return Vec::new();
    }
    let discovered: FxHashMap<&Path, &DiscoveredFile> = files.iter().map(|f| (f.path.as_path(), f)).collect();
    let mut unmatched = Vec::new();
    for path in explicit {
        match discovered.get(path.as_path()) {
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
}
