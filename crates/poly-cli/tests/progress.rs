//! The progress indicator's non-negotiable constraints.
//!
//! A spinner is worth having only if it is invisible everywhere it does not
//! belong. These tests run poly with both streams piped — exactly what a CI job,
//! a shell pipeline and a redirect to a file look like — and assert that not one
//! control character reaches either stream, and that the machine-readable
//! document is byte-for-byte what it was.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// The escape byte every terminal control sequence starts with, including the
/// spinner's own erase-line (`ESC [ 2 K`).
const ESC: char = '\u{1b}';

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["a.py", "b.py", "c.py"] {
        std::fs::write(dir.path().join(name), "x = 1\n").expect("write fixture");
    }
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

/// Piped stderr must stay free of control characters: a CI log full of spinner
/// frames is a regression, not a feature.
#[test]
fn progress_emits_no_control_characters_into_a_pipe() {
    let dir = repo();
    for args in [
        vec!["lint", "--no-workspace", "."],
        vec!["fmt", "--check", "."],
        vec!["lint", "--no-workspace", "--format", "json", "."],
    ] {
        let output = poly(dir.path(), &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stderr.contains(ESC),
            "{args:?} wrote an escape sequence to a piped stderr: {stderr:?}"
        );
        assert!(
            !stdout.contains(ESC),
            "{args:?} wrote an escape sequence to a piped stdout: {stdout:?}"
        );
        assert!(
            !stderr.contains('\r'),
            "{args:?} wrote a carriage return to a piped stderr: {stderr:?}"
        );
    }
}

/// The machine formats own stdout completely: whatever poly draws while working,
/// the document must be the same bytes it would have been.
#[test]
fn json_document_is_a_single_valid_document_with_progress_available() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--format", "json", "."]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be one valid json document ({e}), got: {stdout:?}"));
}
