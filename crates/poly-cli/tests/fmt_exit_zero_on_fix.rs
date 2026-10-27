//! `poly fmt --fix` exit codes: 1 after a rewrite by default, 0 with
//! `--exit-zero-on-fix`, so a `stage_fixed` hook can run it directly.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.py"), "x   =   1\n").expect("write a.py");
    dir
}

fn fmt(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("fmt")
        .args(args)
        .arg("--no-cache")
        .arg("a.py")
        .current_dir(root)
        .output()
        .expect("run poly")
}

#[test]
fn fix_exits_one_after_rewriting_by_default() {
    let dir = repo();
    let output = fmt(dir.path(), &["--fix"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(dir.path().join("a.py")).unwrap(), "x = 1\n");
}

#[test]
fn exit_zero_on_fix_exits_zero_after_rewriting() {
    let dir = repo();
    let output = fmt(dir.path(), &["--fix", "--exit-zero-on-fix"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(dir.path().join("a.py")).unwrap(), "x = 1\n");
}

#[test]
fn exit_zero_on_fix_requires_fix() {
    let dir = repo();
    let output = fmt(dir.path(), &["--exit-zero-on-fix"]);
    assert_eq!(output.status.code(), Some(2));
}
