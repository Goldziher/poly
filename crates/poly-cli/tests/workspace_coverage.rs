//! `poly lint` must not contradict itself about what it linted.
//!
//! The per-file tier holds no Rust rules, so a `.rs` file leaves it uncovered.
//! But `poly lint` also runs a whole-project phase, and that phase runs
//! `cargo clippy`. The first release of the coverage accounting reported both:
//! 229 lines of `skipped …: no lint rules for Rust`, and then, in the same
//! output, `✓ cargo-clippy`. Rust *was* linted. "No lint rules for Rust" is a
//! true statement about the per-file tier and a false one about the run — which
//! is this project's defining defect inverted, a claim that something was not
//! checked when it was.
//!
//! The other half matters just as much: with the whole-project phase off
//! (`--no-workspace`, or a repo that configures no whole-project tools) nothing
//! lints Rust, so the skip is accurate and has to survive.
//!
//! These shell out to the built binary because the contradiction only exists in
//! the assembled output — both phases, one stream.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// The claim under test, verbatim.
const NO_RUST_RULES: &str = "no lint rules for Rust";

/// A whole-project phase that runs one tool, under the id poly's own cargo
/// builtin uses.
///
/// The command is `true` rather than a real `cargo clippy` invocation on
/// purpose: what is under test is whether the two phases agree about coverage,
/// not whether clippy works, and compiling a crate per case would cost seconds
/// to prove nothing extra. `cargo = false` keeps the real cargo builtin group
/// out, so the tool set is exactly the one written here.
const WORKSPACE_HOOKS: &str = r#"
[hooks]
stages = ["pre-commit"]

[hooks.builtin]
cargo = false

[hooks.pre-commit.commands.cargo-clippy]
run = "true"
workspace = true
"#;

fn write(dir: &TempDir, name: &str, body: &str) {
    std::fs::write(dir.path().join(name), body).expect("write fixture");
}

/// A repo with no `[hooks]` section at all, so the whole-project phase has
/// nothing to run and `lib.rs` is the only file in the walk.
fn repo_without_workspace_phase() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir, "lib.rs", "pub fn main() {}\n");
    dir
}

/// The same `lib.rs`, plus the config that gives the run a whole-project phase.
/// The `poly.toml` is itself a file the per-file tier lints, which is why the
/// counts below are two rather than one.
fn repo_with_clippy() -> TempDir {
    let dir = repo_without_workspace_phase();
    write(&dir, "poly.toml", WORKSPACE_HOOKS);
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

/// The defect: a run whose whole-project phase lints Rust must not report Rust
/// as unlinted. Both halves are asserted together — the absence of the claim
/// only means something beside the evidence that the phase did run.
#[test]
fn a_run_that_lints_rust_in_the_whole_project_phase_does_not_call_rust_unlinted() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert!(
        text.contains("✓ cargo-clippy"),
        "the whole-project phase must have run for this test to mean anything, got:\n{text}"
    );
    assert!(
        !text.contains(NO_RUST_RULES),
        "the run linted Rust and must not say otherwise, got:\n{text}"
    );
}

/// Not merely quieter: the file is genuinely counted, so the note, the count,
/// the JSON payload and `--deny-skips` all describe the same run. Suppressing
/// the line while leaving the count at one would be the display-only fix this
/// release exists to rule out.
#[test]
fn a_rust_file_covered_by_the_whole_project_phase_is_counted_as_linted() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text.lines().next(),
        Some("No issues found. (2 file(s) linted)"),
        "lib.rs and poly.toml, nothing skipped, got:\n{text}"
    );
}

/// The same claim, machine-readable: the JSON document must not carry a skip
/// entry the human output has stopped printing.
#[test]
fn json_carries_no_rust_skip_when_the_whole_project_phase_covers_it() {
    let dir = repo_with_clippy();
    let output = poly(
        dir.path(),
        &["lint", "--no-cache", "--no-color", "--format", "json", "."],
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("stdout stays valid JSON");

    assert_eq!(
        value.as_array().expect("top level stays an array").len(),
        0,
        "a covered file with no findings is not an entry: {stdout}"
    );
    assert!(
        !combined(&output).contains(NO_RUST_RULES),
        "got:\n{}",
        combined(&output)
    );
}

/// `--deny-skips` must agree with the note. A skip suppressed for display only
/// would still fail this gate, which is how a display-only fix gets caught.
#[test]
fn deny_skips_passes_when_the_whole_project_phase_covers_the_language() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "--deny-skips", "."]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "the language was linted, so there is no skip to deny: {}",
        combined(&output)
    );
}

/// The other half. With the phase off nothing in the run lints Rust, so the skip
/// is accurate and must still appear, name the file, and stay out of the count.
#[test]
fn no_workspace_keeps_the_rust_skip_because_nothing_lints_rust_then() {
    let dir = repo_with_clippy();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "No issues found. (1 file(s) linted, 1 skipped (no lint rules for Rust))\n",
            "  skipped ./lib.rs: no lint rules for Rust\n"
        )
    );
}

/// A repo that configures no whole-project tools at all reaches the same state
/// without the flag: the phase does not run, so nothing lints Rust — and with
/// nothing left to count, the headline says so rather than reading as clean.
#[test]
fn a_repo_with_no_hooks_config_keeps_the_rust_skip() {
    let dir = repo_without_workspace_phase();
    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "Nothing was linted. (0 file(s) linted, 1 skipped (no lint rules for Rust))\n",
            "  skipped ./lib.rs: no lint rules for Rust\n"
        )
    );
}

/// `--deny-skips` fires on the accurate skip, so the strict gate keeps working
/// for the case it was built for.
#[test]
fn deny_skips_still_fires_on_rust_without_the_whole_project_phase() {
    let dir = repo_with_clippy();
    let output = poly(
        dir.path(),
        &[
            "lint",
            "--no-workspace",
            "--no-cache",
            "--no-color",
            "--deny-skips",
            ".",
        ],
    );
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("error: skipped ./lib.rs: no lint rules for Rust"),
        "got:\n{text}"
    );
}

/// A language *nothing* in the run lints keeps its skip even while the
/// whole-project phase covers another language. Crediting the phase must not
/// become a blanket amnesty.
#[test]
fn a_language_nothing_lints_keeps_its_skip_beside_a_covered_one() {
    let dir = repo_with_clippy();
    write(&dir, "a.kt", "fun main() {}\n");

    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text.lines().next(),
        Some("No issues found. (2 file(s) linted, 1 skipped (no lint rules for Kotlin))"),
        "Rust is covered, Kotlin is not, got:\n{text}"
    );
    assert!(
        text.contains("  skipped ./a.kt: no lint rules for Kotlin"),
        "got:\n{text}"
    );
    assert!(!text.contains(NO_RUST_RULES), "got:\n{text}");
}

/// …and `--deny-skips` still sees it, which is the whole reason an uncovered
/// language has to stay in the skipped set rather than merely be mentioned.
#[test]
fn deny_skips_fires_on_kotlin_while_the_whole_project_phase_covers_rust() {
    let dir = repo_with_clippy();
    write(&dir, "a.kt", "fun main() {}\n");

    let output = poly(dir.path(), &["lint", "--no-cache", "--no-color", "--deny-skips", "."]);
    let text = combined(&output);

    assert_eq!(output.status.code(), Some(2), "got:\n{text}");
    assert!(
        text.contains("error: skipped ./a.kt: no lint rules for Kotlin"),
        "got:\n{text}"
    );
    assert!(
        text.contains("refusing to report success for 1 skipped file(s)"),
        "the covered Rust file must not be in the failing set, got:\n{text}"
    );
}

/// Nine Kotlin files and nothing else, so the note has exactly one reason to
/// report.
fn kotlin_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..9 {
        write(&dir, &format!("a{i}.kt"), "fun main() {}\n");
    }
    dir
}

/// The note groups by reason: many files sharing one reason collapse to a count
/// and a sample instead of one line each. 229 identical lines is not a report.
#[test]
fn a_bulk_reason_is_aggregated_in_the_end_to_end_note() {
    let dir = kotlin_repo();
    let output = poly(dir.path(), &["lint", "--no-workspace", "--no-cache", "--no-color", "."]);
    let text = combined(&output);

    assert_eq!(
        text,
        concat!(
            "Nothing was linted. (0 file(s) linted, 9 skipped (no lint rules for Kotlin))\n",
            "  skipped 9 file(s): no lint rules for Kotlin\n",
            "    e.g. ./a0.kt, ./a1.kt, ./a2.kt — pass --verbose to list them, ",
            "or --format json for the full set\n"
        )
    );
}

/// `--verbose` opts out of the grouping and names every file, so the aggregated
/// view never becomes the only view.
#[test]
fn verbose_expands_the_aggregated_note() {
    let dir = kotlin_repo();
    let output = poly(
        dir.path(),
        &["lint", "--no-workspace", "--no-cache", "--no-color", "--verbose", "."],
    );
    let text = combined(&output);

    assert_eq!(
        text.matches("  skipped ").count(),
        9,
        "one line per file under --verbose, got:\n{text}"
    );
    assert!(
        text.contains("  skipped ./a8.kt: no lint rules for Kotlin"),
        "got:\n{text}"
    );
}
