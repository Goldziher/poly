//! Folding project findings into the per-file results.

use std::path::PathBuf;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::{FixMode, ProjectFinding, ProjectOutcome, fixes};
use crate::engine::Diagnostic;
use crate::filter::{PerFileIgnores, Suppressions, relative_for_match};
use crate::runner::SuppressedDiagnostic;
use crate::runner::types::LintResult;

/// One file's project findings, from every engine that reported on it, before `--fix` acts on them.
struct Pending {
    config_id: usize,
    loaded: Option<Arc<str>>,
    diagnostics: Vec<Diagnostic>,
    suppressed: Vec<SuppressedDiagnostic>,
}

/// Fold project findings into the per-file results, applying inline `poly: allow` directives and
/// `[per-file-ignores]` exactly as the per-file tier does, then `--fix`.
///
/// A finding for a file the per-file tier never produced a result for (it was skipped, or lies
/// outside the discovered set) gets a result of its own. Findings of one file from several engines
/// are fixed together, so the edits of all of them are applied against the same content.
pub(super) fn merge_findings(
    linted: &mut Vec<LintResult>,
    outcome: ProjectOutcome,
    ignores: &[PerFileIgnores],
    ignore_bases: &[Vec<PathBuf>],
    fix: FixMode,
) {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut pending: FxHashMap<PathBuf, Pending> = FxHashMap::default();
    for finding in outcome.findings {
        let ProjectFinding {
            path,
            config_id,
            mut diagnostics,
            loaded,
            ..
        } = finding;
        let content = loaded
            .clone()
            .or_else(|| std::fs::read_to_string(&path).ok().map(Arc::from));
        let mut suppressed: Vec<SuppressedDiagnostic> = Vec::new();
        if let Some(content) = &content {
            let directives = Suppressions::parse(content);
            if !directives.is_empty() {
                directives.apply_directives(&path, &mut diagnostics, &mut suppressed);
            }
        }
        if let Some(this_ignores) = ignores.get(config_id).filter(|set| !set.is_empty()) {
            let bases = ignore_bases.get(config_id).map_or(&[][..], Vec::as_slice);
            let rel = relative_for_match(&path, bases);
            this_ignores.apply(&rel, &path, &mut diagnostics, &mut suppressed);
        }
        match pending.get_mut(&path) {
            Some(entry) => {
                // Edits are offsets into the content their engine read; a different read cannot share a commit.
                if entry.loaded != loaded {
                    for diagnostic in &mut diagnostics {
                        diagnostic.fix.clear();
                    }
                }
                entry.diagnostics.append(&mut diagnostics);
                entry.suppressed.append(&mut suppressed);
            }
            None => {
                order.push(path.clone());
                pending.insert(
                    path,
                    Pending {
                        config_id,
                        loaded,
                        diagnostics,
                        suppressed,
                    },
                );
            }
        }
    }
    let mut results = Vec::new();
    for path in order {
        let Some(Pending {
            config_id,
            loaded,
            mut diagnostics,
            suppressed,
        }) = pending.remove(&path)
        else {
            continue;
        };
        let applied = fixes::resolve(&path, loaded.as_deref(), &mut diagnostics, fix);
        if diagnostics.is_empty() && suppressed.is_empty() && applied.fixed == 0 {
            continue;
        }
        results.push(LintResult {
            path,
            config: config_id,
            diagnostics,
            suppressed,
            fix_withheld_generated: applied.withheld_generated,
            fixed: applied.fixed,
            skipped: None,
            error: None,
            debug: None,
        });
    }
    fold(linted, results);
}

/// Add `results` to `linted`, joining a file the per-file tier already has a result for.
pub(super) fn fold(linted: &mut Vec<LintResult>, results: Vec<LintResult>) {
    let mut index: FxHashMap<PathBuf, usize> = linted.iter().enumerate().map(|(i, r)| (r.path.clone(), i)).collect();
    for mut result in results {
        match index.get(&result.path) {
            Some(&at) => {
                linted[at].diagnostics.append(&mut result.diagnostics);
                linted[at].suppressed.append(&mut result.suppressed);
                linted[at].fixed += result.fixed;
                linted[at].fix_withheld_generated |= result.fix_withheld_generated;
            }
            None => {
                index.insert(result.path.clone(), linted.len());
                linted.push(result);
            }
        }
    }
}
