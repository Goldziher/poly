//! `poly fmt` exit-code matrix. A successful `--fix` exits 0; `--exit-nonzero-on-fix`
//! makes a rewrite exit 1; a dry run that finds drift exits 1; a file the formatter
//! cannot process exits 2 regardless.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.py"), "x=1\n").expect("write a.py");
    std::fs::write(dir.path().join("clean.py"), "x = 1\n").expect("write clean.py");
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn fix_that_changed_files_exits_zero() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--fix", "a.py"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(rewritten(dir.path()));
}

#[test]
fn fix_with_exit_nonzero_on_fix_that_changed_files_exits_one() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--fix", "--exit-nonzero-on-fix", "a.py"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(rewritten(dir.path()), "the files are still rewritten");
}

#[test]
fn dry_run_with_changes_exits_one_and_writes_nothing() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "a.py"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(!rewritten(dir.path()));
}

#[test]
fn clean_tree_exits_zero_in_every_mode() {
    let dir = repo();
    for args in [
        vec!["fmt", "clean.py"],
        vec!["fmt", "--fix", "clean.py"],
        vec!["fmt", "--fix", "--exit-nonzero-on-fix", "clean.py"],
    ] {
        let output = poly(dir.path(), &args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn exit_nonzero_on_fix_does_not_hide_an_unformattable_file() {
    let dir = repo();
    std::fs::write(dir.path().join("bad.py"), "def (:\n").expect("write bad.py");
    let output = poly(dir.path(), &["fmt", "--fix", "--exit-nonzero-on-fix", "bad.py"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn exit_nonzero_on_fix_requires_fix() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--exit-nonzero-on-fix", "a.py"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!rewritten(dir.path()));
}

#[test]
fn removed_exit_zero_on_fix_is_rejected() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--fix", "--exit-zero-on-fix", "a.py"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!rewritten(dir.path()), "a rejected run must not write");
}

#[test]
fn check_is_removed_with_a_hint() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--check", "a.py"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("drop --check"), "{}", stderr(&output));
    assert!(!rewritten(dir.path()));
}
