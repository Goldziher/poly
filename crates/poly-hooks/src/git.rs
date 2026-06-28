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
//! `clone_repo`, `get_lfs_files`, `get_ancestors_not_in_remote`,
//! `get_git_dir`, `get_git_common_dir`, `get_git_hooks_dir`, `write_tree`,
//! `ls_files`, `has_diff`, `files_not_staged`, `has_unmerged_paths`,
//! `is_in_merge_conflict`, `get_conflicted_files`.

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
pub static GIT_ENV_TO_REMOVE: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
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

    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim_ascii(),
    ))
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

/// List files changed between `old` and `new` (merge-base or direct range).
#[instrument(level = "trace")]
pub fn get_changed_files(old: &str, new: &str, root: &Path) -> Result<Vec<PathBuf>, Error> {
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

/// Return the path to the `.git` directory (or git-dir for worktrees).
pub fn get_git_dir() -> Result<PathBuf, Error> {
    let output = git_cmd("get git dir")?
        .arg("rev-parse")
        .arg("--git-dir")
        .check(true)
        .output()?;
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim_ascii(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zsplit_splits_on_nul_and_filters_empty() {
        let input = b"a/b.rs\0c/d.rs\0\0";
        let result = zsplit(input).expect("valid utf-8");
        assert_eq!(
            result,
            vec![PathBuf::from("a/b.rs"), PathBuf::from("c/d.rs")]
        );
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
}
