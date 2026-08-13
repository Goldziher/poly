//! Revision and history queries — everything that names a commit.
//!
//! Split from the parent module because these helpers share an input class the
//! index queries do not have: revisions arriving from untrusted stdin (the
//! pre-push hook's remote/local SHA pairs). They therefore share one guard,
//! [`validate_revision`], which keeps a value that git would parse as an option
//! out of the argument vector. Keeping them together keeps that guard next to
//! every call site it exists for.

use std::path::{Path, PathBuf};

use tracing::instrument;

use super::{Error, git_cmd, zsplit};
use crate::process::Cmd;

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

    let output = build_cmd(format!("{old}...{new}"))?.check(false).output()?;
    if output.status.success() {
        return Ok(zsplit(&output.stdout)?);
    }

    let output = build_cmd(format!("{old}..{new}"))?.check(true).output()?;
    Ok(zsplit(&output.stdout)?)
}

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
        .arg("-e")
        .arg(rev)
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

    use tempfile::TempDir;

    use crate::git::testutil::{commit_file, init_temp_repo};

    #[test]
    fn revision_functions_reject_option_like_input() {
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
