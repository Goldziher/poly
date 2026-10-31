//! Which index entries a staged snapshot materializes.
//!
//! A snapshot of a whole 90k-file tree costs about a minute and a gigabyte per
//! worktree, and only whole-workspace tools (`cargo clippy`, a type checker) read
//! most of it. A run whose hooks are all per-file needs far less: the staged
//! files themselves, the tracked files that sit beside them and in every
//! directory above them (where a formatter or linter looks for its config, its
//! package markers and its sibling modules), and whatever a hook's command line
//! names. Everything else is not read, so it is not written.
//!
//! Everything else is **mirrored**, not written: each directory outside the
//! selection appears in the snapshot as a real directory whose tracked files are
//! hardlinks to the same path in the live worktree (see [`super::links`]). Tools
//! probe the tree by existence as well as by content — ruff's isort decides a
//! module is first-party by finding `armis/common/lib` on disk, and without it a
//! staged file's import blocks are reflowed differently from the full tree — and
//! a plain directory walk must see the files too, which a directory symlink hid
//! from every walker that does not follow one (Goldziher/poly#45). A mirror costs
//! one inode reference per tracked file and no byte copy. Nothing reached through
//! a mirror is a staged file or a sibling of one, so nothing the run validates is
//! read from the worktree.
//!
//! A mirror prefers the worktree copy because a hardlink is free, but a tracked
//! entry the worktree does not have is materialized from its index blob by the
//! same checkout as the selected entries. Every `skip-worktree` entry is
//! nevertheless **selected** and written from the index, with its directory chain
//! like a staged file, because the worktree copy is not evidence of its content:
//! in a sparse checkout it is absent outright; after
//! `git update-index --skip-worktree` it holds bytes git no longer compares with
//! the index. That costs one write per omitted entry on the first refresh, the
//! price every snapshot paid before scoping existed, and later refreshes only
//! stat them; a checkout without the bit pays nothing.
//!
//! The content of every selected entry still comes from the index blob, so the
//! partially-staged guarantee is unchanged: a path is either absent from the
//! snapshot or byte-identical to what the commit would capture.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::git::{self, StagedEntry};
use crate::model::{Hook, HookCommand};

/// How much of the index a snapshot materializes.
#[derive(Debug, Clone, Copy)]
pub enum SnapshotScope<'a> {
    /// Every staged entry. Required by whole-workspace hooks.
    Full,
    /// The staged `files`, their directory chain's direct entries, the tracked
    /// paths in `pinned`, and every `skip-worktree` entry with its directory chain.
    Sparse {
        /// Repo-relative staged files the run validates.
        files: &'a [PathBuf],
        /// Repo-relative path prefixes to include whole, see [`referenced_paths`].
        pinned: &'a [String],
    },
}

impl SnapshotScope<'_> {
    /// Directory name under the per-repo cache dir. Kept apart per scope so
    /// switching scope never makes one snapshot prune the other's files.
    pub(super) fn subdir(self) -> &'static str {
        match self {
            Self::Full => "staged",
            Self::Sparse { .. } => "staged-sparse",
        }
    }

    pub(super) fn select(self, staged: &[StagedEntry]) -> Selection {
        let Self::Sparse { files, pinned } = self else {
            return Selection {
                entries: staged.to_vec(),
                links: Vec::new(),
            };
        };
        let wanted: HashSet<&Path> = files.iter().map(PathBuf::as_path).collect();
        let pinned: HashSet<PathBuf> = pinned
            .iter()
            .map(|token| PathBuf::from(token.trim_end_matches('/')))
            .filter(|path| git::is_safe_relative_path(path))
            .collect();
        let is_pinned = |path: &Path| path.ancestors().any(|ancestor| pinned.contains(ancestor));

        // A `skip-worktree` entry is selected like a staged file: a link cannot
        // expose it, since the worktree either lacks it (a sparse checkout) or
        // holds bytes git no longer compares with the index.
        let omitted = staged
            .iter()
            .filter(|entry| entry.skip_worktree)
            .map(|entry| entry.path.as_path());
        let mut dirs: HashSet<&Path> = HashSet::from([Path::new("")]);
        for ancestors in files
            .iter()
            .map(PathBuf::as_path)
            .chain(pinned.iter().map(PathBuf::as_path))
            .chain(omitted)
            .map(Path::ancestors)
        {
            dirs.extend(ancestors.skip(1));
        }
        let mut pinned_dirs: HashSet<PathBuf> = HashSet::new();
        for entry in staged {
            if is_pinned(&entry.path) {
                pinned_dirs.extend(entry.path.parent().map(Path::to_path_buf));
            }
        }
        let entries: Vec<StagedEntry> = staged
            .iter()
            .filter(|entry| {
                let parent = entry.path.parent().unwrap_or(Path::new(""));
                wanted.contains(entry.path.as_path())
                    || dirs.contains(parent)
                    || pinned_dirs.contains(parent)
                    || is_pinned(&entry.path)
            })
            .cloned()
            .collect();
        let links = unselected_roots(staged, &entries);
        Selection { entries, links }
    }
}

/// The shallowest directories holding tracked files but nothing selected: the
/// ones the snapshot links to the worktree instead of writing.
fn unselected_roots(staged: &[StagedEntry], selected: &[StagedEntry]) -> Vec<PathBuf> {
    let mut real: HashSet<&Path> = HashSet::from([Path::new("")]);
    for entry in selected {
        real.extend(entry.path.ancestors().skip(1));
    }
    let mut links: Vec<PathBuf> = Vec::new();
    for entry in staged {
        if links.last().is_some_and(|last| entry.path.starts_with(last)) {
            continue;
        }
        let mut prefix = PathBuf::new();
        let mut components = entry.path.components().peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }
            prefix.push(component);
            if !real.contains(prefix.as_path()) {
                links.push(prefix);
                break;
            }
        }
    }
    links.sort();
    links.dedup();
    links
}

/// What [`SnapshotScope::select`] chose.
pub(super) struct Selection {
    /// The entries to materialize, content from the index.
    pub(super) entries: Vec<StagedEntry>,
    /// Directories to mirror from the worktree rather than write, see the module docs.
    pub(super) links: Vec<PathBuf>,
}

/// Path-like words in the hooks' command lines, for [`SnapshotScope::Sparse`]'s
/// `pinned`: a script a hook runs lives in the repository, and the snapshot is
/// the hook's working directory.
///
/// Over-approximate by design. A word that names nothing tracked selects nothing,
/// and one that does select something costs a few files.
#[must_use]
pub fn referenced_paths(hooks: &[Hook]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for hook in hooks {
        match &hook.command {
            HookCommand::Run(line) => words.extend(line.split_whitespace().map(str::to_owned)),
            HookCommand::Script { path, .. } => words.push(path.clone()),
        }
        words.extend(hook.args.iter().cloned());
        words.extend(
            hook.before
                .iter()
                .flat_map(|step| step.split_whitespace().map(str::to_owned)),
        );
        words.extend(
            hook.precondition
                .iter()
                .flat_map(|step| step.split_whitespace().map(str::to_owned)),
        );
    }
    let mut paths: Vec<String> = words
        .iter()
        .flat_map(|word| word.split(['=', ':']))
        .map(|word| {
            word.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | ';' | '&' | '|' | '<' | '>' | '`'))
        })
        .map(|word| word.strip_prefix("./").unwrap_or(word))
        .filter(|word| !word.is_empty() && !word.starts_with(['-', '/', '$', '~']) && *word != "." && *word != "..")
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}
