//! Which repository, and which checkout of it, a cache slot belongs to.
//!
//! A repository and a checkout are different things once `git worktree` is in
//! play: every linked worktree is its own checkout of one repository, and they
//! all share the repository's *common dir* (the main checkout's `.git`). The
//! result cache is content-addressed — entries are keyed by the bytes and the
//! checkout-relative path — so it is keyed by the repository and shared by all
//! of its worktrees. The hook staged snapshot is per-run working state that a
//! commit rewrites in place, so it stays keyed by the checkout: two worktrees
//! committing at once must never materialize into one directory.
//!
//! The common dir is resolved by reading git's own files rather than spawning
//! `git`, because this runs on every `poly lint` / `poly fmt` before any engine:
//!
//! - `<checkout>/.git` is a directory (a main checkout): that directory is the
//!   git dir;
//! - `<checkout>/.git` is a file (a linked worktree or a submodule): its
//!   `gitdir: <path>` line names the git dir, relative to the checkout;
//! - `<git dir>/commondir`, when present (linked worktrees), names the common
//!   dir, relative to the git dir; otherwise the git dir is the common dir.
//!
//! Anything unreadable falls back to the checkout itself, which is the keying
//! poly used before worktrees were shared.

use std::path::{Path, PathBuf};

/// The git common dir of the checkout rooted at `checkout`, canonicalized, or
/// `None` when `checkout` has no readable `.git` entry.
///
/// A submodule resolves to its own `.git/modules/<name>` directory, so it never
/// shares a slot with the superproject.
pub fn git_common_dir(checkout: &Path) -> Option<PathBuf> {
    let dot_git = checkout.join(".git");
    let metadata = std::fs::metadata(&dot_git).ok()?;
    let git_dir = if metadata.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        if target.is_empty() {
            return None;
        }
        checkout.join(target)
    };
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) if !text.trim().is_empty() => git_dir.join(text.trim()),
        _ => git_dir,
    };
    dunce::canonicalize(&common).ok().filter(|dir| dir.is_dir())
}

/// The path the shared result cache is keyed by for the checkout rooted at
/// `checkout`: its git common dir, else the checkout itself.
pub fn repository_identity(checkout: &Path) -> PathBuf {
    git_common_dir(checkout).unwrap_or_else(|| checkout.to_path_buf())
}

/// The per-checkout cache directory for the checkout containing `start`:
/// `<cache_home>/<checkout-key>`, keyed by the checkout root itself. The hook
/// staged snapshot lives here, under `staged/`.
///
/// Unlike the result cache, the snapshot is working state a commit rewrites in
/// place, so two worktrees committing at once must never share it.
///
/// The directory is created owner-only exactly as [`repo_cache_dir`](crate::repo_cache_dir) does: a
/// snapshot of the repository's source can never land in a directory other
/// local users can traverse — even when the result cache is disabled and never
/// creates its own tree.
pub fn checkout_cache_dir(start: &Path) -> anyhow::Result<PathBuf> {
    let anchor = crate::repo_anchor(start);
    let (home, origin) = crate::cache_home_with_origin()?;
    Ok(crate::ensure_slot(home.join(crate::repo_key(&anchor)), origin).0)
}

/// What a result cache keeps at its root. Before the result cache was shared
/// by worktrees, it lived in the per-checkout slot beside the staged snapshot.
const LEGACY_RESULT_ENTRIES: [&str; 5] = ["results", "index", "VERSION", "LAST_SWEEP", ".lock"];

/// Remove the result cache an older poly kept in `checkout_slot`, now that the
/// result cache lives at `shared_root`.
///
/// Best-effort migration hygiene, like the legacy in-repo `.polylint/`: only the
/// result-cache entries go, never the staged snapshot beside them, and nothing
/// happens when the two slots are one directory (a checkout outside git).
pub(crate) fn remove_legacy_results(checkout_slot: &Path, shared_root: &Path) {
    if checkout_slot == shared_root || !checkout_slot.join("results").is_dir() {
        return;
    }
    for entry in LEGACY_RESULT_ENTRIES {
        let path = checkout_slot.join(entry);
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(path = %path.display(), "could not remove a legacy result cache: {error}");
        }
    }
}

/// The checkout a [`ResultCache`](crate::ResultCache) opened for, so per-path
/// records can be keyed relative to it and told apart from another worktree's.
#[derive(Debug)]
pub(crate) struct Checkout {
    /// The root as the run reaches it, then its canonical form when that
    /// differs: a file path is matched against whichever prefix it carries.
    roots: Vec<PathBuf>,
    id: u64,
}

impl Checkout {
    pub(crate) fn new(root: &Path) -> Self {
        let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let canonical = dunce::canonicalize(&absolute).unwrap_or_else(|_| absolute.clone());
        let hash = blake3::hash(canonical.as_os_str().as_encoded_bytes());
        let mut id = [0u8; 8];
        id.copy_from_slice(&hash.as_bytes()[..8]);
        let mut roots = vec![absolute];
        if roots[0] != canonical {
            roots.push(canonical);
        }
        Self {
            roots,
            id: u64::from_le_bytes(id),
        }
    }

    /// `absolute` relative to the checkout root, or `None` outside it.
    pub(crate) fn relative<'p>(&self, absolute: &'p Path) -> Option<&'p Path> {
        self.roots.iter().find_map(|root| absolute.strip_prefix(root).ok())
    }

    /// A stable identifier for this checkout.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The directory under `index/` holding this checkout's stat indexes.
    pub(crate) fn index_dir_name(&self) -> String {
        format!("{:016x}", self.id)
    }
}

/// The file name of `namespace`'s stat index.
pub(crate) fn index_file_name(namespace: crate::Namespace) -> String {
    format!("{}.idx", namespace.as_dir())
}

/// The most recently written stat index of `namespace` from another checkout
/// of the repository, to seed a checkout whose own index is `own`
/// (`<root>/index/<checkout>/<namespace>.idx`).
pub(crate) fn newest_sibling_index(own: &Path, namespace: crate::Namespace) -> Option<PathBuf> {
    let own_dir = own.parent()?;
    let index_root = own_dir.parent()?;
    let name = index_file_name(namespace);
    std::fs::read_dir(index_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir != own_dir)
        .map(|dir| dir.join(&name))
        .filter_map(|file| Some((std::fs::metadata(&file).ok()?.modified().ok()?, file)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, file)| file)
}

/// Every stat index of `namespace` under `<root>/index`: the top-level one a
/// cache without a checkout writes, and one per checkout.
pub(crate) fn index_files(root: &Path, namespace: crate::Namespace) -> Vec<PathBuf> {
    let index_root = root.join("index");
    let name = index_file_name(namespace);
    let per_checkout = std::fs::read_dir(&index_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|dir| dir.join(&name));
    std::iter::once(index_root.join(&name))
        .chain(per_checkout)
        .filter(|file| file.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo_with_commit(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "-qm", "init"]);
    }

    #[test]
    fn a_main_checkout_resolves_to_its_dot_git() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        repo_with_commit(&repo);

        assert_eq!(
            git_common_dir(&repo),
            Some(dunce::canonicalize(repo.join(".git")).unwrap())
        );
    }

    #[test]
    fn linked_worktrees_share_the_main_checkouts_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        repo_with_commit(&repo);
        let linked = tmp.path().join("linked");
        git(
            &repo,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "other"],
        );

        let main = git_common_dir(&repo).expect("main common dir");
        assert_eq!(git_common_dir(&linked).as_ref(), Some(&main));
        assert_eq!(repository_identity(&linked), repository_identity(&repo));
    }

    #[test]
    fn a_relative_gitdir_and_commondir_are_resolved_against_their_own_files() {
        let tmp = tempfile::tempdir().unwrap();
        let common = tmp.path().join("store");
        let git_dir = common.join("worktrees").join("w");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("commondir"), "../..\n").unwrap();
        let checkout = tmp.path().join("w");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join(".git"), "gitdir: ../store/worktrees/w\n").unwrap();

        assert_eq!(git_common_dir(&checkout), Some(dunce::canonicalize(&common).unwrap()));
    }

    #[test]
    fn a_submodule_without_commondir_keys_by_its_own_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let modules = tmp.path().join("super/.git/modules/sub");
        std::fs::create_dir_all(&modules).unwrap();
        let checkout = tmp.path().join("super/sub");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        assert_eq!(git_common_dir(&checkout), Some(dunce::canonicalize(&modules).unwrap()));
    }

    #[test]
    fn a_directory_without_git_keys_by_itself() {
        let tmp = tempfile::tempdir().unwrap();

        assert_eq!(git_common_dir(tmp.path()), None);
        assert_eq!(repository_identity(tmp.path()), tmp.path());
    }

    #[test]
    fn a_dangling_gitdir_falls_back_to_the_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".git"), "gitdir: ./missing\n").unwrap();

        assert_eq!(git_common_dir(tmp.path()), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_checkout_resolves_to_the_same_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        repo_with_commit(&repo);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();

        assert_eq!(git_common_dir(&link), git_common_dir(&repo));
    }

    #[test]
    fn a_legacy_result_cache_in_the_checkout_slot_is_removed_but_the_snapshot_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("checkout-slot");
        for dir in ["results/lint", "index", "staged/src", "staged-sparse"] {
            std::fs::create_dir_all(slot.join(dir)).unwrap();
        }
        std::fs::write(slot.join("VERSION"), "5").unwrap();
        std::fs::write(slot.join("LAST_SWEEP"), "").unwrap();

        remove_legacy_results(&slot, &tmp.path().join("shared"));

        for gone in ["results", "index", "VERSION", "LAST_SWEEP"] {
            assert!(!slot.join(gone).exists(), "{gone} must be removed");
        }
        assert!(slot.join("staged/src").is_dir(), "the staged snapshot is never touched");
        assert!(slot.join("staged-sparse").is_dir());
    }

    #[test]
    fn a_slot_that_is_the_shared_root_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("results")).unwrap();

        remove_legacy_results(tmp.path(), tmp.path());

        assert!(tmp.path().join("results").is_dir());
    }

    #[test]
    fn checkout_paths_are_relative_to_either_root_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = Checkout::new(tmp.path());
        let canonical = dunce::canonicalize(tmp.path()).unwrap();

        assert_eq!(checkout.relative(&tmp.path().join("a/b.rs")), Some(Path::new("a/b.rs")));
        assert_eq!(checkout.relative(&canonical.join("a/b.rs")), Some(Path::new("a/b.rs")));
        assert_eq!(checkout.relative(Path::new("/elsewhere/b.rs")), None);
    }
}
