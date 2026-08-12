//! End-to-end coverage for path arguments that do not resolve.
//!
//! A missing path used to be discarded silently by the walker, so
//! `poly fmt --check typo.py` printed `All formatted. (0 file(s) scanned)` and
//! exited 0 — a green result that verified nothing. The mixed case was worse: a
//! list of real and missing paths checked only the real ones, still exited 0,
//! and produced a plausible-looking file count with no signal at all. A hook or
//! CI step feeding poly a stale path list was indistinguishable from a passing
//! gate.
//!
//! These shell out to the built binary so they cover arg parsing → validation →
//! exit code, which is the layer the guarantee actually lives at.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A repo with one genuinely unformatted Python file.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.py"), "x   =    1\n").expect("write ok.py");
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

#[test]
fn missing_path_fails_instead_of_reporting_success() {
    let dir = repo();
    for subcommand in [
        vec!["fmt", "--check", "does-not-exist.py"],
        vec!["lint", "--no-workspace", "does-not-exist.py"],
    ] {
        let output = poly(dir.path(), &subcommand);

        assert_eq!(
            output.status.code(),
            Some(2),
            "{subcommand:?} must fail, not report a clean tree"
        );
        assert!(
            stderr(&output).contains("path does not exist: does-not-exist.py"),
            "{subcommand:?} must name the missing path, got: {}",
            stderr(&output)
        );
    }
}

/// The dangerous case: enough real paths that the file count looks plausible,
/// with the missing ones silently dropped.
#[test]
fn missing_path_mixed_with_real_ones_still_fails() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--check", "ok.py", "nope.py"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("nope.py"));
}

#[test]
fn every_missing_path_is_named_not_just_the_first() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--check", "a.py", "b.py", "c.py"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = stderr(&output);
    for name in ["a.py", "b.py", "c.py"] {
        assert!(stderr.contains(name), "{name} missing from stderr: {stderr}");
    }
}

/// Paths that do resolve must behave exactly as before: this is a guard against
/// the validation rejecting legitimate runs.
#[test]
fn resolvable_paths_are_unaffected() {
    let dir = repo();

    let file = poly(dir.path(), &["fmt", "--check", "ok.py"]);
    assert_eq!(file.status.code(), Some(1), "unformatted file still reports drift");

    let directory = poly(dir.path(), &["fmt", "--check", "."]);
    assert_eq!(directory.status.code(), Some(1));
}
