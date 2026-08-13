//! End-to-end coverage for files `poly lint` could not process at all.
//!
//! A per-file engine failure used to be logged at `warn` and dropped from the
//! results, so the file vanished from the run and `poly lint` printed `No issues
//! found. (1 file(s) linted)` and exited 0 on a file it had never read. That is a
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
    std::fs::write(dir.path().join("ok.py"), "print(\"hi\")\n").expect("write ok.py");
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

/// The reported defect: the run failed on the only file it was given and said
/// it had found nothing.
#[test]
fn errored_file_is_named_and_fails_the_run() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "bad.py"]);
    let text = combined(&output);

    assert_eq!(
        output.status.code(),
        Some(2),
        "a file the engine could not process was not verified, got:\n{text}"
    );
    assert!(text.contains("bad.py"), "the failing path must be named, got:\n{text}");
    assert!(
        text.contains("1 file(s) could not be linted and were NOT checked."),
        "got:\n{text}"
    );
    assert!(
        !text.contains("No issues found."),
        "a run that failed on its only file found nothing because it looked at nothing, got:\n{text}"
    );
}

/// An error is not a skip. The skipped file is reported as skipped, the errored
/// file as an error, and neither borrows the other's wording or accounting.
#[test]
fn an_engine_error_is_not_reported_as_a_skip() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "bad.py", "App.csproj", "ok.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("skipped App.csproj: no matching engine for this file type"),
        "the unmatched path is still a skip, got:\n{text}"
    );
    assert!(
        !text.contains("skipped bad.py"),
        "an engine failure must not be laundered into a skip, got:\n{text}"
    );
    assert!(
        text.contains("1 skipped (no matching engine for this file type)"),
        "the skip count must not absorb the errored file, got:\n{text}"
    );
    assert!(
        text.contains("1 file(s) linted"),
        "only the readable file was linted, got:\n{text}"
    );
    assert!(
        text.contains("1 file(s) could not be linted and were NOT checked."),
        "got:\n{text}"
    );
}

/// The skip budget covers skips. An errored file fails the run on its own
/// account, and must not be counted against `--max-skips`, or the two categories
/// become indistinguishable to a consumer tuning the budget.
#[test]
fn an_engine_error_does_not_consume_the_skip_budget() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--max-skips", "0", "bad.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        !text.contains("refusing to report success for"),
        "the failure is an engine error, not a skip-budget breach, got:\n{text}"
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
    assert!(text.contains("No issues found. (1 file(s) linted)"), "got:\n{text}");
    assert!(!text.contains("could not be linted"), "got:\n{text}");
}

/// Mixed run: one clean file, one skipped, one errored — all three reported,
/// each as itself.
#[test]
fn mixed_run_reports_clean_skipped_and_errored_distinctly() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--format", "json", "."],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert_eq!(output.status.code(), Some(2), "got:\n{stdout}{stderr}");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {stdout}"));
    let entries = value.as_array().expect("top level stays an array");

    let errored = entries
        .iter()
        .find(|entry| entry["path"].as_str().is_some_and(|p| p.ends_with("bad.py")))
        .unwrap_or_else(|| panic!("the errored file must be carried structurally: {stdout}"));
    assert!(
        errored["error"].is_string(),
        "the error must be machine-readable: {stdout}"
    );
    assert!(
        errored["skipped"].is_null(),
        "an errored file is not a skipped one: {stdout}"
    );
    assert_eq!(
        errored["diagnostics"].as_array().map(Vec::len),
        Some(0),
        "a file that could not be read has no findings: {stdout}"
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

/// `--fix` must not report its fixes in a way that reads as a successful run
/// when a file errored: the run is incomplete, whatever it managed to fix.
#[test]
fn fix_does_not_imply_success_when_a_file_errored() {
    let dir = repo();
    std::fs::write(dir.path().join("ok.py"), "import os\n\nprint(\"hi\")\n").expect("write ok.py");

    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--fix", "ok.py", "bad.py"],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("Lint did not complete."),
        "the headline must state the run is incomplete, got:\n{text}"
    );
    assert!(
        text.contains("Fixed 1 issue(s) in 1 file(s)."),
        "what the run did is still reported, got:\n{text}"
    );
    assert!(!text.contains("No issues found."), "got:\n{text}");
}
