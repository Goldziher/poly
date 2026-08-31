//! `poly lint --only` / `--skip` end to end: arg parsing → runner → report →
//! exit code.
//!
//! Two properties carry the flag, and neither is visible from a unit test.
//!
//! A narrowed run must stay usable under `--deny-skips`. Filtering empties the
//! plan for every language the caller did not name, and an empty plan already
//! meant "no matching engine for this file type" — a reason that is false here
//! and that `--deny-skips` turns into exit 2. A gate is the *first* place
//! someone reaches for `--only`, so failing there would make the flag useless
//! exactly where it is wanted.
//!
//! And naming engines must not drag in the whole-project phase. `--only typos`
//! exists to be cheap; escalating to `cargo clippy` would defeat it.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A Python file and a Rust file: under `--only ruff` the first is checked and
/// the second is left unselected, which is the pair every assertion here needs.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).expect("write fixture");
    write("a.py", "x = 1\n");
    write("main.rs", "fn main() {}\n");
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

#[test]
fn a_narrowed_run_is_not_charged_for_what_it_deselected() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--only", "ruff", "--deny-skips", "."]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a deselected file must not fail the skip budget: {}",
        combined(&output)
    );
}

/// The budget still fires on a file poly genuinely cannot check, so narrowing
/// suppresses only its own reason and not the gate itself.
#[test]
fn the_skip_budget_still_fires_on_an_unhandled_file() {
    let dir = repo();
    std::fs::write(dir.path().join("App.csproj"), "<Project />\n").expect("write fixture");
    let output = poly(dir.path(), &["lint", "--only", "ruff", "--deny-skips", "App.csproj"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unmatched explicit path must still fail: {}",
        combined(&output)
    );
}

#[test]
fn naming_engines_skips_the_whole_project_phase_and_says_so() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--only", "typos", "."]);
    let text = combined(&output);
    assert!(
        text.contains("whole-project phase skipped"),
        "the run must state that it dropped the phase: {text}"
    );
}

#[test]
fn an_unknown_engine_name_fails_instead_of_checking_nothing() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--only", "ruffs", "."]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a typo must not read as a clean run: {}",
        combined(&output)
    );
    let text = combined(&output);
    assert!(text.contains("ruffs"), "the error must quote the input back: {text}");
}

/// `--only` and `--skip` ask opposite questions, so clap rejects the pair rather
/// than letting one silently win.
#[test]
fn only_and_skip_are_mutually_exclusive() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--only", "ruff", "--skip", "typos", "."]);
    assert_ne!(output.status.code(), Some(0), "{}", combined(&output));
}

/// The list form is one flag, comma-separated — the shape the issue asked for.
#[test]
fn a_comma_separated_list_selects_every_name_in_it() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--only", "ruff,typos", "--deny-skips", "."]);
    assert_eq!(output.status.code(), Some(0), "{}", combined(&output));
}
