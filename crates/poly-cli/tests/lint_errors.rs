//! End-to-end coverage for files `poly lint` could not process at all.
//!
//! A per-file engine failure used to be logged at `warn` and dropped from the
//! results, so the file vanished from the run and `poly lint` printed `No issues
//! found. (1 file linted)` and exited 0 on a file it had never read. That is a
//! gate that passes without checking — the same defect `poly fmt` already fixed
//! with `FormatRun::errors`.
//!
//! An engine error is a third category, distinct from a skip: a skip is poly
//! correctly declining a file it does not handle (exit code unchanged), an error
//! is poly failing on a file it accepted (exit 2, "not verified"). These tests
//! assert the two are never conflated.
//!
//! The failure is induced with a `.py` file holding invalid UTF-8: the runner
//! reads every file as text before any engine sees it, so the error is
//! deterministic, parallel-safe, and independent of the host toolchain.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A `.csproj` is routed nowhere: no poly engine claims the extension, so it is
/// a *skip*, not an error.
const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n  </PropertyGroup>\n</Project>\n";

/// Bytes that are not valid UTF-8, in a file poly does route to an engine.
const INVALID_UTF8: &[u8] = b"x = 1\n\xff\xfe not utf-8\n";

/// A repo with one file poly lints cleanly, one it cannot read, and one no
/// engine covers — the three outcomes that must stay distinguishable.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // Clean under poly's *whole* default ruff selection — a bare `print(...)`
    // stopped qualifying once `T20` joined the default set.
    std::fs::write(dir.path().join("ok.py"), "x = 1\n").expect("write ok.py");
    std::fs::write(dir.path().join("bad.py"), INVALID_UTF8).expect("write bad.py");
    std::fs::write(dir.path().join("App.csproj"), CSPROJ).expect("write App.csproj");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Invalid text is accounted for as an actionable error without turning one
/// file into a fatal run error.
#[test]
fn invalid_utf8_is_named_and_fails_without_aborting_the_run() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "bad.py"]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(1), "got:\n{text}");
    assert!(text.contains("bad.py"), "the failing path must be named, got:\n{text}");
    assert!(
        text.contains("error") && text.contains("invalid-utf8") && text.contains("byte 6"),
        "got:\n{text}"
    );
    assert!(
        text.contains("skipped bad.py: file is not valid UTF-8; text linting was skipped"),
        "the file must remain explicitly accounted for as uninspected, got:\n{text}"
    );
    assert!(
        !text.contains("could not be linted"),
        "invalid UTF-8 is not a fatal error, got:\n{text}"
    );
}

/// The decode error and the unrelated unmatched path remain distinct skips.
#[test]
fn invalid_utf8_and_unmatched_paths_keep_distinct_skip_reasons() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "bad.py", "App.csproj", "ok.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(1), "got:\n{text}");
    assert!(
        text.contains("skipped App.csproj: no matching engine for this file type"),
        "the unmatched path is still a skip, got:\n{text}"
    );
    assert!(
        text.contains("skipped bad.py: file is not valid UTF-8; text linting was skipped"),
        "the decode failure must explain why text linting was skipped, got:\n{text}"
    );
    assert!(
        text.contains("no matching engine for this file type") && text.contains("file is not valid UTF-8"),
        "the skip summary must preserve both reasons, got:\n{text}"
    );
    assert!(
        text.contains("1 file linted"),
        "only the readable file was linted, got:\n{text}"
    );
    assert!(!text.contains("could not be linted"), "got:\n{text}");
}

/// A file explicitly marked uninspected remains subject to the skip budget.
#[test]
fn invalid_utf8_consumes_the_skip_budget() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--max-skips", "0", "bad.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("refusing to report success for"),
        "the file was not inspected and must breach a zero skip budget, got:\n{text}"
    );
}

/// The common path is untouched: a run over readable files still exits 0 and
/// gains no error narration.
#[test]
fn a_clean_run_is_unaffected() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "ok.py"]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(0), "got:\n{text}");
    assert!(text.contains("No issues found.\n  1 file linted"), "got:\n{text}");
    assert!(!text.contains("could not be linted"), "got:\n{text}");
}

/// Mixed run: one clean file, one unmatched file, and one decode error — each
/// remains structurally distinguishable.
#[test]
fn mixed_run_reports_clean_skipped_and_errored_distinctly() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--format", "json", "."],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert_eq!(output.status.code(), Some(1), "got:\n{stdout}{stderr}");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {stdout}"));
    let entries = value.as_array().expect("top level stays an array");

    let invalid = entries
        .iter()
        .find(|entry| entry["path"].as_str().is_some_and(|p| p.ends_with("bad.py")))
        .unwrap_or_else(|| panic!("the invalid UTF-8 file must be carried structurally: {stdout}"));
    assert!(
        invalid["error"].is_null(),
        "invalid UTF-8 is not a fatal engine error: {stdout}"
    );
    assert!(
        invalid["skipped"]
            .as_str()
            .is_some_and(|reason| reason.contains("UTF-8")),
        "the uninspected reason must be machine-readable: {stdout}"
    );
    assert_eq!(
        invalid["diagnostics"][0]["code"].as_str(),
        Some("invalid-utf8"),
        "the error must be machine-readable: {stdout}"
    );

    // A directory walk does not narrate unmatched files, so `App.csproj` is
    // absent here by design — the clean file is present only if it had findings,
    // which it does not. What matters is that neither carries an `error`.
    for entry in entries {
        if entry["path"].as_str().is_some_and(|p| p.ends_with("bad.py")) {
            continue;
        }
        assert!(
            entry["error"].is_null(),
            "only the failing file carries an error: {stdout}"
        );
    }
    assert!(
        stderr.contains("bad.py"),
        "the human echo of the failure goes to stderr under --format json: {stderr}"
    );
}

/// `--fix` may repair readable files while preserving the invalid-text error.
#[test]
fn fix_preserves_invalid_utf8_error_while_fixing_readable_files() {
    let dir = repo();
    std::fs::write(dir.path().join("ok.py"), "import os\n\nprint(\"hi\")\n").expect("write ok.py");

    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--fix", "ok.py", "bad.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(1), "got:\n{text}");
    assert!(
        text.contains("invalid-utf8") && text.contains("bad.py"),
        "the error must remain visible, got:\n{text}"
    );
    assert!(
        text.contains("Fixed 1 issue in 1 file"),
        "what the run did is still reported, got:\n{text}"
    );
    assert!(!text.contains("Lint did not complete."), "got:\n{text}");
}
