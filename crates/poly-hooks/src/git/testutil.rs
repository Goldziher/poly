//! Throwaway-repository helpers shared by the `git` submodules' tests.
//!
//! A separate file so the index and revision tests can both drive a real
//! temporary repository without either copy of the setup drifting from the
//! other. Compiled only under `cfg(test)`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

/// Run `git <args>` in `repo`, asserting success and returning trimmed stdout.
pub(crate) fn git_run(repo: &Path, args: &[&str]) -> String {
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

/// Initialize an empty repository in a fresh temporary directory, with signing
/// off and an identity configured so commits succeed on any machine.
pub(crate) fn init_temp_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    git_run(path, &["init", "-q"]);
    git_run(path, &["config", "user.email", "test@example.com"]);
    git_run(path, &["config", "user.name", "Test"]);
    git_run(path, &["config", "commit.gpgsign", "false"]);
    dir
}

/// Write, stage, and commit a one-line file named `name`, returning the new HEAD.
pub(crate) fn commit_file(repo: &Path, name: &str) -> String {
    std::fs::write(repo.join(name), name).expect("write file");
    git_run(repo, &["add", name]);
    git_run(repo, &["commit", "-q", "-m", name]);
    git_run(repo, &["rev-parse", "HEAD"])
}
