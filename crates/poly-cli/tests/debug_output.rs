//! Log lines on stderr: plain text whenever the report would be, and no spurious warnings.
//!
//! The tracing subscriber styled every line unconditionally, so a piped run, a CI
//! log and a `--color never` run all received raw ANSI escape codes.

use std::process::{Command, Output};

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn debug_lint(extra: &[&str]) -> Output {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.py"), "x = 1\n").expect("write fixture");
    Command::new(POLY)
        .args(extra)
        .args(["--debug", "lint", "--no-cache", "a.py"])
        .current_dir(dir.path())
        .env_remove("NO_COLOR")
        .env_remove("RUST_LOG")
        .output()
        .expect("run poly")
}

fn assert_plain_debug_logs(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DEBUG"),
        "the run must log at debug level, stderr was:\n{stderr}"
    );
    assert!(
        !stderr.contains('\u{1b}'),
        "stderr carries ANSI escape codes:\n{stderr:?}"
    );
}

#[test]
fn piped_debug_logs_carry_no_escape_codes() {
    assert_plain_debug_logs(&debug_lint(&[]));
}

#[test]
fn color_never_debug_logs_carry_no_escape_codes() {
    assert_plain_debug_logs(&debug_lint(&["--color", "never"]));
}

#[test]
fn color_always_styles_debug_logs_even_when_piped() {
    let output = debug_lint(&["--color", "always"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains('\u{1b}'), "--color always must style logs:\n{stderr}");
}

/// A `.txt` file goes to the generic tier as `text`, which has no grammar in the
/// pack. Asking the pack to prefetch it failed and logged a warning on every run.
#[test]
fn a_language_without_a_grammar_is_not_prefetched() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("notes.txt"), "plain text\n").expect("write fixture");
    let output = Command::new(POLY)
        .args(["--color", "never", "fmt", "--no-cache", "notes.txt"])
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .output()
        .expect("run poly");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr was:\n{stderr}");
    assert!(!stderr.contains("prefetch failed"), "stderr was:\n{stderr}");
}
