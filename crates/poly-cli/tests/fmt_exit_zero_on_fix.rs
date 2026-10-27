//! `poly fmt --fix` exits 1 after reformatting, which a git hook reads as a failed
//! check. `--exit-zero-on-fix` is the hook-facing variant: the files are still
//! rewritten, but the exit code says the run succeeded.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.py"), "x=1\n").expect("write a.py");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn rewritten(root: &Path) -> bool {
    std::fs::read_to_string(root.join("a.py")).expect("read a.py") == "x = 1\n"
}

#[test]
fn fix_alone_exits_one_after_reformatting() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--fix", "a.py"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(rewritten(dir.path()));
}

#[test]
fn exit_zero_on_fix_still_rewrites_but_exits_zero() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--fix", "--exit-zero-on-fix", "a.py"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rewritten(dir.path()));
}

#[test]
fn exit_zero_on_fix_does_not_hide_an_unformattable_file() {
    let dir = repo();
    std::fs::write(dir.path().join("bad.py"), "def (:\n").expect("write bad.py");
    let output = poly(dir.path(), &["fmt", "--fix", "--exit-zero-on-fix", "bad.py"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn exit_zero_on_fix_requires_fix() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--exit-zero-on-fix", "a.py"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!rewritten(dir.path()));
}
