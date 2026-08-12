//! Detecting the files a hook rewrote, and getting a staged-run fix back into
//! the working tree without destroying unstaged work.
//!
//! Two questions the sequential boundary in [`super::run_hooks`] needs answered
//! after a hook exits:
//!
//! 1. *Did it rewrite its own inputs?* — [`Fingerprints`]. This gates
//!    `stage_fixed` re-staging and the result-cache store (an entry may only be
//!    kept for a run that changed nothing, or the next hit would replay a pass
//!    that was only reached by fixing).
//! 2. *Where does the fix go?* — [`write_back`]. Under a staged run the hook
//!    rewrote the **snapshot**, not the worktree, so the fix has to be carried
//!    across — and only where doing so cannot lose work.
//!
//! Detection is content-based rather than stat-based on purpose: a formatter
//! that rewrites a file to the same length within one filesystem mtime tick is
//! not exotic, and a missed rewrite here means either a lost fix or a cached
//! "passed" for content that never passed on its own.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::git;
use crate::model::HookStatus;

use super::HookRun;

/// A blake3 digest of one file's bytes.
type ContentHash = [u8; blake3::OUT_LEN];

/// Content fingerprints of a hook's files, captured before the hook runs so an
/// in-place rewrite can be detected afterwards.
pub(super) struct Fingerprints {
    /// One entry per captured path; `None` when the file could not be read.
    entries: Vec<(PathBuf, Option<ContentHash>)>,
}

impl Fingerprints {
    /// An empty capture — for hooks whose outcome does not depend on whether
    /// they rewrote anything, so no file is ever read for them.
    pub(super) fn none() -> Self {
        Self { entries: Vec::new() }
    }

    /// Fingerprint each repo-relative path in `paths` beneath `root`.
    ///
    /// An unreadable path (a staged deletion, a permissions error) fingerprints
    /// as `None`. `None` compares equal to a later `None` — a file absent before
    /// and after was not touched — and unequal to any hash, so a file the hook
    /// created counts as modified.
    pub(super) fn capture(root: &Path, paths: &[PathBuf]) -> Self {
        Self {
            entries: paths
                .iter()
                .map(|path| (path.clone(), hash_file(&root.join(path))))
                .collect(),
        }
    }

    /// The captured paths whose content differs from what was captured.
    pub(super) fn modified(&self, root: &Path) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|(path, before)| hash_file(&root.join(path)) != *before)
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// Hash a file's bytes, or `None` when it cannot be read.
fn hash_file(path: &Path) -> Option<ContentHash> {
    let mut hasher = blake3::Hasher::new();
    let mut file = fs_err::File::open(path).ok()?;
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(*hasher.finalize().as_bytes())
}

/// Land the rewrites made by a whole priority group, updating each hook's
/// outcome.
///
/// This runs once per group, not once per hook, because the hooks in a group
/// run **concurrently**: two of them can touch the same file, and a per-hook
/// pass would let the first one's write make the second one's write-back look
/// unsafe. Deciding each path exactly once — on the worktree state as it was
/// before the group's rewrites were carried across — keeps the result
/// independent of which hook happened to finish first.
///
/// `stage_fixed[i]` corresponds to `runs[i]`. Only a **passing** hook's
/// rewrites are landed: a failing hook's half-finished output must never reach
/// the index.
pub(super) fn land_group_rewrites(
    root: &Path,
    snapshot: Option<&Path>,
    stage_fixed: &[bool],
    runs: &mut [HookRun],
) -> anyhow::Result<()> {
    let rewritten: BTreeSet<PathBuf> = runs
        .iter()
        .filter(|run| passed(run))
        .flat_map(|run| run.modified.iter().cloned())
        .collect();
    if rewritten.is_empty() {
        return Ok(());
    }

    // A worktree run needs no transfer — the hook already wrote the very files
    // the commit will take — so `stage_fixed` just stages them.
    let Some(snapshot) = snapshot else {
        for (index, run) in runs.iter_mut().enumerate() {
            if stage_fixed[index] && passed(run) && !run.modified.is_empty() {
                git::add(root, &run.modified)?;
                run.outcome.files_modified = true;
            }
        }
        return Ok(());
    };

    let paths: Vec<PathBuf> = rewritten.into_iter().collect();
    let withheld: BTreeSet<PathBuf> = write_back(root, snapshot, &paths)?.into_iter().collect();

    let mut to_stage: BTreeSet<PathBuf> = BTreeSet::new();
    for (index, run) in runs.iter_mut().enumerate() {
        if !stage_fixed[index] || !passed(run) {
            continue;
        }
        let (mine_withheld, mine_applied): (Vec<PathBuf>, Vec<PathBuf>) =
            run.modified.iter().cloned().partition(|path| withheld.contains(path));
        run.outcome.files_modified = !mine_applied.is_empty();
        to_stage.extend(mine_applied);
        if !mine_withheld.is_empty() {
            run.outcome.status = HookStatus::FixWithheld(mine_withheld);
        }
    }
    let staged: Vec<PathBuf> = to_stage.into_iter().collect();
    git::add(root, &staged)?;
    Ok(())
}

/// Whether a hook exited 0 — the only state whose rewrites may be landed.
fn passed(run: &HookRun) -> bool {
    matches!(run.outcome.status, HookStatus::Passed)
}

/// Carry each rewrite made in `snapshot` back into the worktree at `root`,
/// returning the paths it **refused** to touch. Staging is the caller's
/// decision (`stage_fixed`); this only moves bytes.
///
/// The rewrite was computed from **staged** bytes, which makes the write safe in
/// exactly one case: the worktree copy is byte-identical to the index, so it
/// holds nothing the rewrite has not already seen. Then writing it reproduces
/// the pre-isolation result exactly — the file the author sees is the fixed one.
///
/// Where the worktree copy differs, the author is holding unstaged work. Writing
/// the staged-derived fix over it would destroy that work outright, and staging
/// the file would commit hunks they deliberately left out. Neither is a call
/// this runner may make on their behalf, so the path is withheld and the caller
/// decides what that means for the hook's verdict.
fn write_back(root: &Path, snapshot: &Path, modified: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut withheld = Vec::new();
    for path in modified {
        if git::has_worktree_diff_in(root, path)? {
            withheld.push(path.clone());
            continue;
        }
        fs_err::copy(snapshot.join(path), root.join(path))?;
    }
    Ok(withheld)
}
