//! End-to-end coverage for what `poly lint --fix` says about the files it
//! rewrote.
//!
//! `poly lint --fix` used to print `No issues found.` while rewriting files on
//! disk: the run reported on what it *found* afterwards rather than on what it
//! *did*. A consumer whose autofix silently deleted content had no line of
//! output pointing at the run that did it, which is a large part of why the loss
//! went unnoticed. Check mode was always correct (`2 issue(s) found. … 2 fixable
//! with the --fix option.`); only the `--fix` path was silent.
//!
//! These assert on the *effect* — the printed summary — because a test that only
//! checked the file contents would have passed against the broken output.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// Two unused imports: ruff reports both as autofixable, and the fix rewrites
/// the file.
const UNUSED_IMPORTS: &str = "import os\nimport sys\n\nprint(\"hi\")\n";

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("unused.py"), UNUSED_IMPORTS).expect("write unused.py");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The reported defect: files were rewritten, the summary said nothing happened.
#[test]
fn fix_reports_what_it_rewrote_instead_of_claiming_nothing_was_found() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--fix", "unused.py"],
    );
    let text = stdout(&output);

    assert!(
        std::fs::read_to_string(dir.path().join("unused.py"))
            .expect("read back")
            .trim()
            == "print(\"hi\")",
        "precondition: the fix must actually rewrite the file"
    );
    assert!(
        !text.contains("No issues found."),
        "a run that rewrote a file did not find nothing, got:\n{text}"
    );
    assert!(
        text.contains("Fixed 2 issue(s) in 1 file(s)."),
        "the summary must name what was fixed, got:\n{text}"
    );
}

/// A run that fixed some issues and left others must report both halves: the
/// remaining findings are what the caller acts on, the fixes are what changed
/// under them.
#[test]
fn fix_reports_both_what_it_fixed_and_what_remains() {
    let dir = repo();
    // `undefined_name` has no autofix, so one finding survives the fix pass.
    std::fs::write(dir.path().join("unused.py"), "import os\n\nprint(undefined_name)\n").expect("write unused.py");

    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--fix", "unused.py"],
    );
    let text = stdout(&output);

    assert!(text.contains("issue(s) found."), "got:\n{text}");
    assert!(text.contains("Fixed 1 issue(s) in 1 file(s)."), "got:\n{text}");
}

/// A `--fix` run with nothing to fix keeps the old summary exactly: no new
/// noise on the clean path.
#[test]
fn fix_with_nothing_to_do_still_reports_a_clean_run() {
    let dir = repo();
    std::fs::write(dir.path().join("unused.py"), "print(\"hi\")\n").expect("write unused.py");

    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--fix", "unused.py"],
    );
    let text = stdout(&output);

    assert!(text.contains("No issues found."), "got:\n{text}");
    assert!(!text.contains("Fixed"), "got:\n{text}");
}

/// Check mode must not claim to have fixed anything — it wrote nothing.
#[test]
fn check_mode_reports_fixable_findings_not_fixed_ones() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "unused.py"]);
    let text = stdout(&output);

    assert!(text.contains("2 fixable with the `--fix` option."), "got:\n{text}");
    assert!(!text.contains("Fixed"), "got:\n{text}");
}

/// The fixed count is carried structurally too, so a machine consumer can see
/// which files a `--fix` run rewrote instead of diffing the tree.
#[test]
fn json_output_carries_the_fixed_count_per_file() {
    let dir = repo();
    let output = poly(
        dir.path(),
        &[
            "lint",
            "--no-workspace",
            "--no-cache",
            "--fix",
            "--format",
            "json",
            "unused.py",
        ],
    );
    let text = stdout(&output);
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let entries = value.as_array().expect("top level stays an array");
    let entry = entries
        .iter()
        .find(|entry| entry["path"] == "unused.py")
        .unwrap_or_else(|| panic!("the rewritten file must appear: {text}"));
    assert_eq!(entry["fixed"], 2, "got: {text}");
}
