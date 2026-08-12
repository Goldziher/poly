//! `poly lint` path scoping for the whole-project phase.
//!
//! `poly lint some/file.py` used to run the per-file tier and then escalate to
//! the whole-project phase — `cargo clippy` and friends across the entire
//! workspace. Nothing in the argument list distinguished that from a sub-second
//! per-file check, so a path-scoped lint issued while another process held the
//! cargo package lock blocked indefinitely and silently. Two agents in one
//! reporting repo concluded poly was broken; one was killed at 13 minutes.
//!
//! Explicit paths now scope the run, with `--workspace` to opt back in.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");
const NOTE: &str = "whole-project phase skipped for path-scoped run";

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.py"), "x = 1\n").expect("write ok.py");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The skip is explained, so "this did less than you think" is visible at the
/// moment it happens rather than discovered later.
#[test]
fn explicit_paths_skip_the_whole_project_phase_and_say_so() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "ok.py"]);

    assert!(
        stderr(&output).contains(NOTE),
        "expected the skip note, got: {}",
        stderr(&output)
    );
}

/// `--workspace` opts a path-scoped run back in, for a commit gate that lints
/// staged paths and genuinely wants clippy.
#[test]
fn workspace_flag_opts_back_in() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--workspace", "ok.py"]);

    assert!(!stderr(&output).contains(NOTE));
}

/// A whole-repository run is unchanged — this is the common CI invocation and
/// must keep running the whole-project phase.
#[test]
fn no_paths_still_runs_the_whole_project_phase() {
    let dir = repo();
    let output = poly(dir.path(), &["lint"]);

    assert!(!stderr(&output).contains(NOTE));
}

/// `--no-workspace` is an explicit opt-out and needs no narration; emitting the
/// note there would be pure noise on the flag people are told to use.
#[test]
fn no_workspace_flag_is_not_narrated() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "ok.py"]);

    assert!(!stderr(&output).contains(NOTE));
}

/// The two flags express opposite intents; taking both is a mistake worth
/// reporting rather than silently resolving.
#[test]
fn workspace_and_no_workspace_conflict() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--workspace", "--no-workspace", "ok.py"]);

    assert_eq!(output.status.code(), Some(2), "clap usage error");
}
