//! Synchronous git helper functions.
//!
//! Ported from `polyhooks/src/git.rs`. All `async`/`.await` removed; every
//! call uses the blocking `Cmd::output()` / `Cmd::status()` methods.
//!
//! The following upstream helpers are intentionally **not ported** (clone /
//! fetch helpers, install shims, workspace helpers) — they belong to later
//! phases or the polyhooks crate:
//!
//! `init_repo`, `shallow_clone`, `full_clone`, `clone_repo_attempt`,
//! `clone_repo`, `get_lfs_files`, `write_tree`, `ls_files`, `has_diff`,
//! `files_not_staged`, `has_unmerged_paths`, `is_in_merge_conflict`,
//! `get_conflicted_files`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use tracing::{debug, instrument};

use crate::fs::PathClean as _;
use crate::process::{Cmd, Error as ProcessError, StatusError};

/// Errors returned by git helpers in this module.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Propagated command error.
    #[error(transparent)]
    Command(#[from] ProcessError),

    /// `git` binary not found on `PATH`.
    #[error("Failed to find git: {0}")]
    GitNotFound(#[from] which::Error),

    /// Underlying I/O error (from status / output calls).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Git output was not valid UTF-8.
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),

    /// A revision argument began with `-`, which git would parse as an option.
    #[error("invalid git revision (must not begin with `-`): old={old:?}, new={new:?}")]
    InvalidRevision {
        /// The `old` side of the requested range.
        old: String,
        /// The `new` side of the requested range.
        new: String,
    },
}

/// Resolved path to the `git` binary (or the error from `which::which`).
pub static GIT: LazyLock<Result<PathBuf, which::Error>> = LazyLock::new(|| which::which("git"));

/// Resolved absolute path to the repository root, computed once at startup.
pub static GIT_ROOT: LazyLock<Result<PathBuf, Error>> = LazyLock::new(|| {
    get_root()
        .map(|root| dunce::canonicalize(&root).unwrap_or(root))
        .inspect(|root| {
            debug!("Git root: {}", root.display());
        })
});

/// `GIT_*` environment variables that should be removed when running
/// subprocess git commands to avoid inheriting hook-injected state.
///
/// `GIT_INDEX_FILE` is intentionally kept so that `git write-tree` works
/// correctly when called from inside a `git commit -a` / `-p` hook.
pub static GIT_ENV_TO_REMOVE: LazyLock<Vec<String>> = LazyLock::new(|| {
    const KEEP: &[&str] = &[
        "GIT_EXEC_PATH",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_SSL_CAINFO",
        "GIT_SSL_NO_VERIFY",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_HTTP_PROXY_AUTHMETHOD",
        "GIT_ALLOW_PROTOCOL",
        "GIT_ASKPASS",
    ];

    std::env::vars()
        .filter(|(k, _)| {
            k.starts_with("GIT_")
                && !k.starts_with("GIT_CONFIG_KEY_")
                && !k.starts_with("GIT_CONFIG_VALUE_")
                && !KEEP.contains(&k.as_str())
        })
        .map(|(key, _)| key)
        .collect()
});

/// Build a base `Cmd` pointing at the `git` binary, with
/// `core.useBuiltinFSMonitor=false` injected to avoid monitor overhead.
pub fn git_cmd(summary: &str) -> Result<Cmd, Error> {
    let mut cmd = Cmd::new(GIT.as_ref().map_err(|&e| Error::GitNotFound(e))?, summary);
    cmd.arg("-c").arg("core.useBuiltinFSMonitor=false");
    Ok(cmd)
}

// ── Internal path helpers ─────────────────────────────────────────────────────

fn zsplit(s: &[u8]) -> Result<Vec<PathBuf>, std::str::Utf8Error> {
    s.split(|&b| b == b'\0')
        .filter(|slice| !slice.is_empty())
        .map(path_from_git_bytes)
        .collect()
}

#[cfg(unix)]
#[expect(clippy::unnecessary_wraps)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, std::str::Utf8Error> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    Ok(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, std::str::Utf8Error> {
    std::str::from_utf8(bytes).map(PathBuf::from)
}

// ── Public git helpers ────────────────────────────────────────────────────────

/// Return the top-level directory of the current working tree.
///
/// Unlike most helpers here, this uses `std::process::Command` directly so it
/// can be called to bootstrap the `GIT_ROOT` static before `Cmd` is fully
/// initialised.
#[instrument(level = "trace")]
pub fn get_root() -> Result<PathBuf, Error> {
    let git = GIT.as_ref().map_err(|&e| Error::GitNotFound(e))?;
    let output = std::process::Command::new(git)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()?;

    if !output.status.success() {
        return Err(Error::Command(ProcessError::Status {
            summary: "get git root".to_string(),
            error: StatusError {
                status: output.status,
                output: Some(output),
            },
        }));
    }

    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_ascii()))
}

/// List files that are staged in the index (excluding deleted files).
#[instrument(level = "trace")]
pub fn get_staged_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("get staged files")?
        .current_dir(root)
        .arg("diff")
        .arg("--cached")
        .arg("--name-only")
        .arg("--diff-filter=ACMRTUXB") // everything except D
        .arg("--no-ext-diff")
        .arg("-z")
        .check(true)
        .output()?;
    Ok(zsplit(&output.stdout)?)
}

/// List every file tracked in the index (`git ls-files`).
///
/// Used by `poly hooks run --all-files` and by `pre-push` over a root-commit
/// push, where the whole tracked tree is checked rather than a diff range.
#[instrument(level = "trace")]
pub fn list_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("list tracked files")?
        .current_dir(root)
        .arg("ls-files")
        .arg("-z")
        .check(true)
        .output()?;
    Ok(zsplit(&output.stdout)?)
}

/// Materialize the **staged** index content into `dest` (staged snapshot).
///
/// Runs `git checkout-index -a -f --prefix=<dest>/`, which writes every entry in
/// the index — i.e. exactly the staged content — beneath `dest`, recreating the
/// repo-relative directory tree. Untracked files and unstaged worktree edits are
/// never written, so the result is a byte-faithful, non-destructive copy of what
/// a commit would capture. `dest` must already exist.
///
/// This is how whole-workspace hooks (`cargo clippy`, type checkers, …) are
/// isolated to staged content without touching the live worktree.
#[instrument(level = "trace")]
pub fn checkout_index_to(root: &Path, dest: &Path) -> Result<(), Error> {
    git_cmd("checkout staged index")?
        .current_dir(root)
        .arg("checkout-index")
        .arg("-a") // all index entries
        .arg("-f") // overwrite any pre-existing files in `dest`
        .arg(prefix_arg(dest))
        .check(true)
        .status()?;
    Ok(())
}

/// Materialize the staged content of specific `paths` into `dest`.
///
/// Like [`checkout_index_to`] but scoped: `git checkout-index -f --prefix=<dest>/
/// -- <paths>`. Used by the incremental staged-snapshot refresh to rewrite only
/// the files whose worktree differs from the index; the rest are copied from the
/// worktree (preserving mtimes). A no-op for an empty `paths`.
#[instrument(level = "trace", skip(paths))]
pub fn checkout_index_paths(root: &Path, dest: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = git_cmd("checkout staged paths")?;
    cmd.current_dir(root)
        .arg("checkout-index")
        .arg("-f")
        .arg(prefix_arg(dest))
        .arg("--");
    for path in paths {
        cmd.arg(path);
    }
    cmd.check(true).status()?;
    Ok(())
}

/// List tracked files whose **worktree** content differs from the **index**
/// (`git diff-files --name-only`), i.e. the staged files carrying an additional
/// unstaged edit. For these the worktree copy is not the staged content, so the
/// snapshot must pull the staged blob rather than copy the worktree file.
#[instrument(level = "trace")]
pub fn worktree_modified_files(root: &Path) -> Result<Vec<PathBuf>, Error> {
    let output = git_cmd("list worktree-modified files")?
        .current_dir(root)
        .arg("diff-files")
        .arg("--name-only")
        .arg("--no-ext-diff")
        .arg("-z")
        .check(true)
        .output()?;
    Ok(zsplit(&output.stdout)?)
}

/// Build the `--prefix=<dest>/` argument for `git checkout-index`.
///
/// The prefix is prepended verbatim to each index path, so it must carry a
/// trailing separator or the first path component would be glued onto `dest`.
fn prefix_arg(dest: &Path) -> std::ffi::OsString {
    let mut prefix = dest.as_os_str().to_os_string();
    prefix.push(std::path::MAIN_SEPARATOR_STR);
    let mut arg = std::ffi::OsString::from("--prefix=");
    arg.push(&prefix);
    arg
}

/// Reject a revision that git would misinterpret as an option.
///
/// Revisions reaching us from untrusted input — notably the SHAs parsed from
/// the pre-push hook's stdin — must never begin with `-`, or git parses them as
/// a flag instead of an object name. That is an argument-injection vector even
/// though we never route these values through a shell (`Cmd::arg`, not `sh -c`).
fn validate_revision(rev: &str) -> Result<(), Error> {
    if rev.starts_with('-') {
        return Err(Error::InvalidRevision {
            old: rev.to_string(),
            new: String::new(),
        });
    }
    Ok(())
}

/// List files changed between `old` and `new` (merge-base or direct range).
#[instrument(level = "trace")]
pub fn get_changed_files(old: &str, new: &str, root: &Path) -> Result<Vec<PathBuf>, Error> {
    // Guard against argument injection: a ref beginning with `-` would be parsed
    // by git as an option rather than as part of the `old...new` range.
    if old.starts_with('-') || new.starts_with('-') {
        return Err(Error::InvalidRevision {
            old: old.to_string(),
            new: new.to_string(),
        });
    }

    let build_cmd = |range: String| -> Result<Cmd, Error> {
        let mut cmd = git_cmd("get changed files")?;
        cmd.arg("diff")
            .arg("--name-only")
            .arg("--diff-filter=ACMRT")
            .arg("--no-ext-diff")
            .arg("-z")
            .arg(range)
            .arg("--")
            .arg(root);
        Ok(cmd)
    };

    // Try three-dot (merge-base) first.
    let output = build_cmd(format!("{old}...{new}"))?.check(false).output()?;
    if output.status.success() {
        return Ok(zsplit(&output.stdout)?);
    }

    // Fall back to two-dot (direct range).
    let output = build_cmd(format!("{old}..{new}"))?.check(true).output()?;
    Ok(zsplit(&output.stdout)?)
}

/// Capture the `git diff` for `path` (worktree vs. index).
///
/// Returns the raw diff bytes. On non-zero exit the diff is still returned
/// (some CI environments have truncated objects — see comment in source).
#[instrument(level = "trace")]
pub fn get_diff(path: &Path) -> Result<Vec<u8>, Error> {
    let output = git_cmd("git diff")?
        .arg("diff")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .arg("--ignore-submodules")
        .arg("--")
        .arg(path)
        .check(false)
        .output()?;

    if !output.status.success() {
        debug!(
            status = %output.status,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "Continuing with git diff stdout despite non-zero exit status"
        );
    }
    Ok(output.stdout)
}

/// Stage `paths` into the index (`git add -- <paths>`).
///
/// Used by `stage_fixed` to re-stage files a hook rewrote. A no-op when
/// `paths` is empty.
#[instrument(level = "trace", skip(paths))]
pub fn add(root: &Path, paths: &[PathBuf]) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = git_cmd("git add")?;
    cmd.current_dir(root).arg("add").arg("--");
    for path in paths {
        cmd.arg(path);
    }
    cmd.check(true).status()?;
    Ok(())
}

/// Return `true` if `path` has any unstaged modifications in the working tree.
#[instrument(level = "trace")]
pub fn has_worktree_diff(path: &Path) -> Result<bool, Error> {
    let mut cmd = git_cmd("check worktree diff")?;
    let status = cmd
        .arg("diff-files")
        .arg("--quiet")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .arg("--ignore-submodules")
        .arg("--")
        .arg(path)
        .check(false)
        .status()?;

    if status.success() {
        return Ok(false);
    }
    if status.code() == Some(1) {
        return Ok(true);
    }

    cmd.check_status(status)?;
    Ok(true)
}

/// Like [`has_worktree_diff`], but runs git inside `root` so the (repo-relative)
/// `path` resolves correctly regardless of the process working directory.
///
/// Used by the runner's `stage_fixed` boundary to detect files a hook rewrote.
#[instrument(level = "trace")]
pub fn has_worktree_diff_in(root: &Path, path: &Path) -> Result<bool, Error> {
    let mut cmd = git_cmd("check worktree diff")?;
    let status = cmd
        .current_dir(root)
        .arg("diff-files")
        .arg("--quiet")
        .arg("--no-ext-diff")
        .arg("--no-textconv")
        .arg("--ignore-submodules")
        .arg("--")
        .arg(path)
        .check(false)
        .status()?;

    if status.success() {
        return Ok(false);
    }
    if status.code() == Some(1) {
        return Ok(true);
    }
    cmd.check_status(status)?;
    Ok(true)
}

/// Return the path to the `.git` directory (or git-dir for worktrees).
pub fn get_git_dir() -> Result<PathBuf, Error> {
    let output = git_cmd("get git dir")?
        .arg("rev-parse")
        .arg("--git-dir")
        .check(true)
        .output()?;
    Ok(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_ascii()))
}

/// Return the git common dir (the primary `.git` even in a linked worktree).
pub fn get_git_common_dir() -> Result<PathBuf, Error> {
    let output = git_cmd("get git common dir")?
        .arg("rev-parse")
        .arg("--git-common-dir")
        .check(true)
        .output()?;
    let trimmed = output.stdout.trim_ascii();
    if trimmed.is_empty() {
        get_git_dir()
    } else {
        Ok(PathBuf::from(String::from_utf8_lossy(trimmed).as_ref()))
    }
}

/// Return the effective git hooks directory (respects `core.hooksPath`).
pub fn get_git_hooks_dir() -> Result<PathBuf, Error> {
    let output = git_cmd("get git hooks dir")?
        .arg("rev-parse")
        .arg("--git-path")
        .arg("hooks")
        .check(true)
        .output()?;

    let hooks_dir = if output.stdout.trim_ascii().is_empty() {
        get_git_common_dir()?.join("hooks")
    } else {
        PathBuf::from(String::from_utf8_lossy(output.stdout.trim_ascii()).as_ref())
    };
    Ok(hooks_dir.clean())
}

// ── Pre-push stdin parsing helpers ─────────────────────────────────────────────

/// Return `true` if `rev` names an existing, valid git object in `root`.
///
/// Used by the pre-push shim to decide whether the remote tip it was handed on
/// stdin is a commit this repository can reason about.
#[instrument(level = "trace")]
pub fn rev_exists(rev: &str, root: &Path) -> Result<bool, Error> {
    validate_revision(rev)?;
    let mut cmd = git_cmd("git cat-file")?;
    let status = cmd
        .current_dir(root)
        .arg("cat-file")
        // Exit 0 if <object> exists and is a valid object, 1 if it does not.
        .arg("-e")
        .arg(rev)
        .check(false)
        .status()?;

    if status.success() {
        return Ok(true);
    }
    // Exit 1 = object absent; any other status (e.g. 128 for a corrupt object
    // store) is a real git error and must not masquerade as "absent".
    if status.code() == Some(1) {
        return Ok(false);
    }

    cmd.check_status(status)?;
    Ok(false)
}

/// Return `true` if `ancestor` is an ancestor of `commit` (via `merge-base`).
///
/// Exit code `0` means yes, `1` means no; any other status is propagated.
#[instrument(level = "trace")]
pub fn is_ancestor(ancestor: &str, commit: &str, root: &Path) -> Result<bool, Error> {
    validate_revision(ancestor)?;
    validate_revision(commit)?;
    let mut cmd = git_cmd("check commit ancestry")?;
    let status = cmd
        .current_dir(root)
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(ancestor)
        .arg(commit)
        .check(false)
        .status()?;

    if status.success() {
        return Ok(true);
    }
    if status.code() == Some(1) {
        return Ok(false);
    }

    cmd.check_status(status)?;
    Ok(false)
}

/// Commits reachable from `local_sha` that no ref of `remote_name` can reach.
///
/// Ordered oldest-first (`--topo-order --reverse`), so the first element is the
/// earliest commit the remote does not already have.
#[instrument(level = "trace")]
pub fn get_ancestors_not_in_remote(local_sha: &str, remote_name: &str, root: &Path) -> Result<Vec<String>, Error> {
    validate_revision(local_sha)?;
    // `remote_name` is safe: it is bound inside `--remotes={remote_name}`, so it
    // can never be parsed as a standalone option even if it begins with `-`.
    let output = git_cmd("get ancestors not in remote")?
        .current_dir(root)
        .arg("rev-list")
        .arg(local_sha)
        .arg("--topo-order")
        .arg("--reverse")
        .arg("--not")
        .arg(format!("--remotes={remote_name}"))
        .check(true)
        .output()?;
    Ok(std::str::from_utf8(&output.stdout)?
        .trim_ascii()
        .lines()
        .map(ToString::to_string)
        .collect())
}

/// Root commits (commits with no parents) reachable from `local_sha`.
#[instrument(level = "trace")]
pub fn get_root_commits(local_sha: &str, root: &Path) -> Result<Vec<String>, Error> {
    validate_revision(local_sha)?;
    let output = git_cmd("get root commits")?
        .current_dir(root)
        .arg("rev-list")
        .arg("--max-parents=0")
        .arg(local_sha)
        .check(true)
        .output()?;
    Ok(std::str::from_utf8(&output.stdout)?
        .trim_ascii()
        .lines()
        .map(ToString::to_string)
        .collect())
}

/// Resolve the first parent of `commit` (`<commit>^`), if any.
///
/// Returns `Ok(None)` when `commit` has no parent (e.g. a root commit).
#[instrument(level = "trace")]
pub fn get_parent_commit(commit: &str, root: &Path) -> Result<Option<String>, Error> {
    // `commit` is interpolated into `{commit}^`, so a leading `-` would still
    // reach `rev-parse` as an option-like token; guard it like the others.
    validate_revision(commit)?;
    let output = git_cmd("get parent commit")?
        .current_dir(root)
        .arg("rev-parse")
        .arg(format!("{commit}^"))
        .check(false)
        .output()?;
    if output.status.success() {
        Ok(Some(std::str::from_utf8(&output.stdout)?.trim_ascii().to_string()))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    use tempfile::TempDir;

    fn git_run(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git invocation");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn revision_functions_reject_option_like_input() {
        // A `-`-prefixed revision (e.g. from a hostile pre-push stdin) must be
        // rejected before it can reach git as an option. The guard fires before
        // any git invocation, so an empty temp dir suffices.
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let evil = "--upload-pack=touch /tmp/pwned";

        assert!(matches!(rev_exists(evil, root), Err(Error::InvalidRevision { .. })));
        assert!(matches!(
            is_ancestor(evil, "HEAD", root),
            Err(Error::InvalidRevision { .. })
        ));
        assert!(matches!(
            is_ancestor("HEAD", evil, root),
            Err(Error::InvalidRevision { .. })
        ));
        assert!(matches!(
            get_ancestors_not_in_remote(evil, "origin", root),
            Err(Error::InvalidRevision { .. })
        ));
        assert!(matches!(
            get_root_commits(evil, root),
            Err(Error::InvalidRevision { .. })
        ));
        assert!(matches!(
            get_parent_commit(evil, root),
            Err(Error::InvalidRevision { .. })
        ));
    }

    fn init_temp_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path();
        git_run(path, &["init", "-q"]);
        git_run(path, &["config", "user.email", "test@example.com"]);
        git_run(path, &["config", "user.name", "Test"]);
        git_run(path, &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn commit_file(repo: &Path, name: &str) -> String {
        std::fs::write(repo.join(name), name).expect("write file");
        git_run(repo, &["add", name]);
        git_run(repo, &["commit", "-q", "-m", name]);
        git_run(repo, &["rev-parse", "HEAD"])
    }

    #[test]
    fn zsplit_splits_on_nul_and_filters_empty() {
        let input = b"a/b.rs\0c/d.rs\0\0";
        let result = zsplit(input).expect("valid utf-8");
        assert_eq!(result, vec![PathBuf::from("a/b.rs"), PathBuf::from("c/d.rs")]);
    }

    #[test]
    fn git_static_resolves() {
        // This test runs in a git checkout, so `git` must be on PATH.
        assert!(GIT.is_ok(), "git not found: {:?}", *GIT);
    }

    #[test]
    fn git_root_is_directory() {
        // We're inside the polylint worktree, so get_root() must succeed.
        let root = get_root().expect("git root");
        assert!(root.is_dir(), "root is not a directory: {}", root.display());
    }

    #[test]
    fn rev_exists_distinguishes_real_and_bogus_revisions() {
        let repo = init_temp_repo();
        let head = commit_file(repo.path(), "a.txt");
        assert!(rev_exists(&head, repo.path()).expect("rev_exists"));
        assert!(!rev_exists("0000000000000000000000000000000000000000", repo.path()).expect("rev_exists"));
    }

    #[test]
    fn is_ancestor_reports_parentage() {
        let repo = init_temp_repo();
        let first = commit_file(repo.path(), "a.txt");
        let second = commit_file(repo.path(), "b.txt");
        assert!(is_ancestor(&first, &second, repo.path()).expect("is_ancestor"));
        assert!(!is_ancestor(&second, &first, repo.path()).expect("is_ancestor"));
    }

    #[test]
    fn get_parent_commit_resolves_first_parent() {
        let repo = init_temp_repo();
        let first = commit_file(repo.path(), "a.txt");
        let second = commit_file(repo.path(), "b.txt");
        let parent = get_parent_commit(&second, repo.path()).expect("parent");
        assert_eq!(parent.as_deref(), Some(first.as_str()));
        // A root commit has no parent.
        assert_eq!(get_parent_commit(&first, repo.path()).expect("parent"), None);
    }

    #[test]
    fn get_root_commits_lists_only_the_root() {
        let repo = init_temp_repo();
        let first = commit_file(repo.path(), "a.txt");
        let second = commit_file(repo.path(), "b.txt");
        let roots = get_root_commits(&second, repo.path()).expect("roots");
        assert_eq!(roots, vec![first]);
    }
}
