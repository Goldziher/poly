//! End-to-end coverage for `poly hooks` driving the native runner in a real
//! temporary git repository.
//!
//! These tests shell out to the built `poly` binary (via `CARGO_BIN_EXE_poly`)
//! and exercise the full path: config discovery → lowering → `poly_hooks::run`
//! → reporter → process exit code, plus `install` / `hook-impl`.
//!
//! The job command lines use POSIX shell syntax (`printf`, `true`, `false`),
//! which the runner feeds to `sh -c`; they would not run under `cmd /C`, so the
//! whole suite is gated to Unix. A Windows equivalent would need `cmd`-syntax
//! commands.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn git(repo: &Path, args: &[&str]) -> String {
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

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    dir
}

fn write(repo: &Path, name: &str, contents: &str) {
    std::fs::write(repo.join(name), contents).expect("write file");
}

fn staged_blob(repo: &Path, name: &str) -> String {
    git(repo, &["show", &format!(":{name}")])
}

fn poly_hooks(repo: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("hooks")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("poly invocation")
}

/// A config with a parallel pre-commit stage: a no-op job plus a `stage_fixed`
/// job that rewrites a staged file.
fn stage_fixed_config(stage_fixed: bool) -> String {
    format!(
        r#"
[hooks.pre-commit]
parallel = true

[[hooks.pre-commit.jobs]]
name = "noop"
run = "true"

[[hooks.pre-commit.jobs]]
name = "fixer"
run = "printf changed > fixed.txt"
stage_fixed = {stage_fixed}
"#
    )
}

#[test]
fn run_pre_commit_runs_all_hooks_and_restages_stage_fixed_change() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit"]);
    let report = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Both hooks appear in the (index-ordered) report.
    assert!(report.contains("noop"), "missing noop:\n{report}");
    assert!(report.contains("fixer"), "missing fixer:\n{report}");
    // The position of `noop` precedes `fixer` (deterministic, non-interleaved).
    let noop_at = report.find("noop").unwrap();
    let fixer_at = report.find("fixer").unwrap();
    assert!(noop_at < fixer_at, "hooks not index-ordered:\n{report}");
    // `stage_fixed` re-staged the rewritten file.
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn stage_fixed_false_leaves_modification_unstaged() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(false));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit"]);
    assert!(output.status.success());
    // The index still holds the original; the worktree holds the rewrite.
    assert_eq!(staged_blob(root, "fixed.txt"), "orig");
    assert_eq!(
        std::fs::read_to_string(root.join("fixed.txt")).unwrap(),
        "changed"
    );
}

#[test]
fn run_with_single_job_forces_serial_and_passes() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    let output = poly_hooks(root, &["run", "pre-commit", "-j", "1"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn failing_job_yields_non_zero_exit() {
    let repo = init_repo();
    let root = repo.path();
    write(
        root,
        "poly.toml",
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "boom"
run = "false"
"#,
    );

    let output = poly_hooks(root, &["run", "pre-commit"]);
    assert!(
        !output.status.success(),
        "a failing job must produce a non-zero exit"
    );
}

#[test]
fn hook_impl_pre_commit_runs_and_restages() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", &stage_fixed_config(true));
    write(root, "fixed.txt", "orig");
    git(root, &["add", "fixed.txt"]);

    // pre-commit takes no git arguments.
    let output = poly_hooks(root, &["hook-impl", "--hook-type=pre-commit", "--"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        report.contains("noop") && report.contains("fixer"),
        "{report}"
    );
    assert_eq!(staged_blob(root, "fixed.txt"), "changed");
}

#[test]
fn install_writes_a_shim_that_git_commit_triggers() {
    let repo = init_repo();
    let root = repo.path();
    // A pre-commit job that records a sentinel proves the shim fired through the
    // native runner.
    write(
        root,
        "poly.toml",
        r#"
[hooks.pre-commit]
[[hooks.pre-commit.jobs]]
name = "sentinel"
run = "touch sentinel.created"
"#,
    );

    let installed = poly_hooks(root, &["install", "--hook-type", "pre-commit"]);
    assert!(
        installed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let shim = root.join(".git/hooks/pre-commit");
    assert!(shim.is_file(), "shim was not written");
    assert!(
        std::fs::read_to_string(&shim)
            .unwrap()
            .contains("hook-impl --hook-type=pre-commit"),
        "shim missing exec line"
    );

    // Commit something; the installed pre-commit shim must fire and run our job.
    write(root, "tracked.txt", "content");
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-q", "-m", "feat: trigger hook"]);

    assert!(
        root.join("sentinel.created").exists(),
        "installed pre-commit shim did not trigger the native runner"
    );
}
