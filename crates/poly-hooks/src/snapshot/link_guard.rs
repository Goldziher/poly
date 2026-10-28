//! Catching a hook that writes through a sparse snapshot's links.
//!
//! A sparse snapshot links every directory it does not write to the same path in
//! the live worktree, so tools that probe the tree by existence still see it. A
//! hook that writes into such a directory is therefore not writing to the
//! snapshot but to the author's working tree — unreviewed, and outside the staged
//! content the gate vouches for. A link cannot be made read-only without
//! changing the worktree itself, so the write is detected instead: each linked
//! directory is fingerprinted (its unstaged diff and its untracked files) before
//! the hooks run and compared afterwards.

use std::path::{Path, PathBuf};

use super::links;
use crate::git;

/// The linked directories of a sparse snapshot and their state before the run.
pub struct LinkGuard {
    root: PathBuf,
    before: Vec<(PathBuf, Option<[u8; 32]>)>,
}

impl LinkGuard {
    /// Fingerprint the directories the snapshot at `snapshot` links to the
    /// worktree at `root`. `None` when it links none (a full snapshot, or a sparse
    /// one that wrote everything).
    #[must_use]
    pub fn arm(root: &Path, snapshot: &Path) -> Option<Self> {
        let linked = links::recorded(snapshot);
        if linked.is_empty() {
            return None;
        }
        let before = linked
            .into_iter()
            .map(|dir| {
                let digest = git::worktree_digest(root, &dir).ok();
                (dir, digest)
            })
            .collect();
        Some(Self {
            root: root.to_path_buf(),
            before,
        })
    }

    /// The linked directories whose worktree state differs from before the run.
    #[must_use]
    pub fn changed(&self) -> Vec<PathBuf> {
        self.before
            .iter()
            .filter(|(dir, before)| before.is_some() && git::worktree_digest(&self.root, dir).ok() != *before)
            .map(|(dir, _)| dir.clone())
            .collect()
    }
}
