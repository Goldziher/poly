//! Git-hook importers: lefthook and pre-commit configs become `[hooks]`,
//! `[hooks.builtin]` and `[tools]` entries in `poly.toml`.
//!
//! Both sources feed one accumulator because their jobs can land in the same
//! poly builtin, and because a lefthook `prek run` job is only redundant once
//! the pre-commit config it runs has been absorbed.

mod acc;
mod command;
mod filters;
mod lefthook;
mod precommit;
mod tree;

use std::path::Path;

use super::{Absorb, Fragment, ImportResult};
use acc::{HookAcc, Origin};

/// Import every hook framework config in `dir`.
pub fn import(dir: &Path) -> Vec<ImportResult> {
    let mut acc = HookAcc::default();
    acc.diag.origin = Origin::Lefthook;
    let lefthook = lefthook::import(dir, &mut acc);
    acc.diag.origin = Origin::PreCommit;
    let pre_commit = precommit::import(dir, &mut acc);
    acc.diag.origin = Origin::Lefthook;

    if let Some(lefthook) = &lefthook {
        for deferred in &lefthook.pre_commit_jobs {
            let absorbed = pre_commit.as_ref().is_some_and(|p| p.fully_absorbed);
            if absorbed {
                acc.diag.note(format!(
                    "{}: `prek run` dropped - the hooks it ran from .pre-commit-config.yaml are now poly hooks",
                    deferred.label
                ));
            } else {
                acc.diag.lossy(format!(
                    "{}: kept as a command; it runs the pre-commit hooks poly could not absorb, and \
                     any it did absorb run twice",
                    deferred.label
                ));
                acc.add_job(&deferred.stage, deferred.job.clone());
            }
        }
    }
    acc.settle();
    let (fragments, diag) = acc.into_fragments();

    let mut results = Vec::new();
    let produced = fragments.iter().any(|f| !f.entries.is_empty());
    let mut fragments = Some(fragments);
    let lefthook_present = lefthook.is_some();
    if let Some(lefthook) = lefthook {
        let mut notes = diag.notes_for(Origin::Lefthook);
        let lossy = diag.lossy_for(Origin::Lefthook);
        if lossy.is_empty() {
            notes.push("run `poly hooks install` to replace the lefthook git hooks".to_string());
        }
        results.push(result(
            "lefthook",
            lefthook.sources,
            fragments.take(),
            produced,
            notes,
            lossy,
        ));
    }
    if let Some(pre_commit) = pre_commit {
        let notes = diag.notes_for(Origin::PreCommit);
        let lossy = diag
            .lossy_for(Origin::PreCommit)
            .into_iter()
            .map(|entry| abbreviate_shared(entry, lefthook_present))
            .collect();
        results.push(result(
            "pre-commit",
            vec![pre_commit.source],
            fragments.take(),
            produced,
            notes,
            lossy,
        ));
    }
    results
}

/// Shared losses are listed in full under the first result; the second names
/// them without repeating the dropped patterns.
fn abbreviate_shared(entry: String, listed_elsewhere: bool) -> String {
    match entry.split_once("; dropped: ") {
        Some((head, _)) if listed_elsewhere => format!("{head} (patterns listed under lefthook)"),
        _ => entry,
    }
}

fn result(
    tool: &'static str,
    sources: Vec<std::path::PathBuf>,
    fragments: Option<Vec<Fragment>>,
    produced: bool,
    notes: Vec<String>,
    lossy: Vec<String>,
) -> ImportResult {
    let fragments = fragments.unwrap_or_default();
    let absorb = if !lossy.is_empty() {
        Absorb::Partial(lossy)
    } else if !produced {
        Absorb::None
    } else {
        Absorb::Full
    };
    ImportResult {
        tool,
        sources,
        fragments,
        notes,
        absorb,
    }
}
