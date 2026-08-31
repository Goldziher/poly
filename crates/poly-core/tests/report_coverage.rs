//! The coverage question a machine consumer has to be able to answer:
//! **was everything I asked about actually checked?**
//!
//! `errors` was promoted to the top of the document precisely so a caller could
//! gate on that without walking every record. `skipped` was not, and there was
//! no count of what had been checked — so a run where every file was skipped
//! produced an empty diagnostic list, no error flag, and read exactly like a
//! clean pass. An agent told to "lint the changed files and fix what it finds"
//! is right to read that as done.

use std::path::Path;

use poly_core::report::{LintDocument, RunSummary};
use poly_core::{Config, LintRun, RunOptions};

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn lint(dir: &Path) -> LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], &Config::default(), &options(), false, false).expect("lint run")
}

fn summary(dir: &Path) -> RunSummary {
    LintDocument::from_run(&lint(dir)).summary
}

/// The headline case: a tree poly has no rules for reports zero checked and a
/// populated skip list, so "found nothing" and "checked nothing" are different
/// values rather than the same empty array.
#[test]
fn a_run_that_checked_nothing_says_so() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("main.zig"), "pub fn main() void {}\n").expect("write");
    let document = LintDocument::from_run(&lint(dir.path()));
    assert_eq!(document.summary.checked, 0, "nothing was linted");
    assert_eq!(document.summary.skipped, 1, "and the run states it");
    assert_eq!(document.skipped.len(), 1, "the skipped set is at the top level");
    assert!(document.errors.is_empty(), "a skip is not a failure");
}

/// The contrast that makes the count meaningful.
#[test]
fn a_run_that_checked_something_counts_it() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("app.py"), "x = 1\n").expect("write");
    assert_eq!(
        summary(dir.path()),
        RunSummary {
            checked: 1,
            skipped: 0,
            errored: 0
        }
    );
}

/// Two facts that together make `results` unusable as a coverage measure, both
/// pinned so neither can be introduced by accident.
///
/// First: a file can be **both** skipped and carry findings. A `.zig` file is
/// skipped for want of Zig rules, yet the cross-cutting backends still run over
/// it and can report a typo — so the same path is in `results` and in `skipped`.
#[test]
fn a_file_can_be_both_skipped_and_carry_findings() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(
        dir.path().join("main.zig"),
        "// recieve the value\npub fn main() void {}\n",
    )
    .expect("write");
    let document = LintDocument::from_run(&lint(dir.path()));
    assert!(
        document
            .results
            .iter()
            .any(|result| !result.diagnostics.is_empty() && result.skipped.is_some()),
        "expected a file that is both skipped and carries findings: {:?}",
        document.results
    );
    assert_eq!(document.summary.checked, 0, "it still was not linted");
}

/// Second, and the reason `checked` has to be stated rather than derived: a file
/// that was checked and found clean produces **no record at all** — `results`
/// holds only files with something to report. So `checked` counts files that
/// appear nowhere in `results`, and no arithmetic over `results` can recover it.
#[test]
fn checked_counts_files_that_appear_in_no_record() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("ok.py"), "x = 1\n").expect("write");
    let document = LintDocument::from_run(&lint(dir.path()));
    assert_eq!(document.summary.checked, 1, "the file was linted");
    assert!(
        document.results.is_empty(),
        "and produced no record: {:?}",
        document.results
    );
}

/// A clean run carries empty lists rather than omitting them, so a consumer can
/// read the same fields on every response.
#[test]
fn a_clean_run_still_reports_its_coverage() {
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(dir.path().join("ok.py"), "x = 1\n").expect("write");
    let document = LintDocument::from_run(&lint(dir.path()));
    assert!(document.errors.is_empty());
    assert!(document.skipped.is_empty());
    assert_eq!(document.summary.checked, 1);
}
