//! End-to-end coverage for the three-way resolution of "does `[discovery]
//! exclude` apply to a path named on the command line?".
//!
//! `--force-exclude` was declared on `CommonArgs` and never read, and the
//! matching `[discovery] force_exclude` key parsed into a field nothing
//! consumed. Both appeared in `--help` and in the generated hook command lines
//! and did nothing; the behaviour was hard-wired to `!--include-excluded`.
//!
//! The load-bearing case is the one that could not pass before: `force_exclude
//! = false` in `poly.toml` must actually check an explicitly named excluded
//! path. Because the *default* is already "exclude it", a test that only asserts
//! the default passes for the wrong reason — so every case here asserts the
//! observable effect (was the file rewritten?) and pairs the excluded file with
//! a non-excluded one to prove the run did work at all.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// Unformatted Python: ruff rewrites this to `x = 1\n`, so "was it checked?" is
/// observable from the file itself rather than from the summary text.
const UNFORMATTED: &str = "x   =    1\n";
const FORMATTED: &str = "x = 1\n";

/// A repo excluding `a.py` and holding an identical, non-excluded `b.py`.
///
/// `discovery` is appended verbatim under `[discovery]`, which is how each test
/// varies only the key under test.
fn repo(discovery: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("poly.toml"),
        format!("[discovery]\nexclude = [\"a.py\"]\n{discovery}"),
    )
    .expect("write poly.toml");
    std::fs::write(dir.path().join("a.py"), UNFORMATTED).expect("write a.py");
    std::fs::write(dir.path().join("b.py"), UNFORMATTED).expect("write b.py");
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

fn read(dir: &TempDir, name: &str) -> String {
    std::fs::read_to_string(dir.path().join(name)).expect("read fixture")
}

/// Format both files and return the combined output. `b.py` is always named so
/// every assertion can prove the run did real work.
fn format_both(dir: &TempDir, extra: &[&str]) -> String {
    let mut args = vec!["fmt", "--fix", "--no-cache"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["a.py", "b.py"]);
    let output = poly(dir.path(), &args);
    let text = combined(&output);
    assert_eq!(
        read(dir, "b.py"),
        FORMATTED,
        "the non-excluded file must always be formatted — otherwise this run did nothing and the \
         excluded-file assertion is vacuous. got:\n{text}"
    );
    text
}

/// The key test: `[discovery] force_exclude = false` makes an explicitly named
/// excluded path checked again. Before the wiring this key parsed and had no
/// reader, so the path stayed excluded and this failed.
#[test]
fn config_force_exclude_false_checks_an_explicitly_named_excluded_path() {
    let dir = repo("force_exclude = false\n");
    let text = format_both(&dir, &[]);

    assert_eq!(
        read(&dir, "a.py"),
        FORMATTED,
        "[discovery] force_exclude = false must check the named excluded path, got:\n{text}"
    );
    assert!(
        !text.contains("matched exclusions"),
        "nothing was excluded, so the exclusion note must not appear, got:\n{text}"
    );
}

/// The CLI flag is the explicit override and beats a config `false`.
#[test]
fn cli_force_exclude_beats_config_force_exclude_false() {
    let dir = repo("force_exclude = false\n");
    let text = format_both(&dir, &["--force-exclude"]);

    assert_eq!(
        read(&dir, "a.py"),
        UNFORMATTED,
        "--force-exclude must override the config's `false`, got:\n{text}"
    );
    assert!(text.contains("matched exclusions"), "got:\n{text}");
}

/// `--include-excluded` is the opposite-direction explicit override and beats a
/// config `true` (and the built-in default).
#[test]
fn include_excluded_beats_config_force_exclude_true() {
    let dir = repo("force_exclude = true\n");
    let text = format_both(&dir, &["--include-excluded"]);

    assert_eq!(
        read(&dir, "a.py"),
        FORMATTED,
        "--include-excluded must override the config's `true`, got:\n{text}"
    );
}

/// With neither flag nor key set, the built-in default holds: the named
/// excluded path is dropped, and the note says so.
#[test]
fn default_force_excludes_an_explicitly_named_excluded_path() {
    let dir = repo("");
    let text = format_both(&dir, &[]);

    assert_eq!(
        read(&dir, "a.py"),
        UNFORMATTED,
        "the default is force-exclude on, got:\n{text}"
    );
    assert!(text.contains("matched exclusions"), "got:\n{text}");
}

/// `force_exclude` governs only *explicitly named* roots. The directory walk
/// applies `exclude` either way, so `force_exclude = false` must not turn
/// `poly fmt .` into a run over excluded files.
#[test]
fn config_force_exclude_false_does_not_widen_the_directory_walk() {
    let dir = repo("force_exclude = false\n");
    let output = poly(dir.path(), &["fmt", "--fix", "--no-cache", "."]);
    let text = combined(&output);

    assert_eq!(read(&dir, "b.py"), FORMATTED, "the walk must still work, got:\n{text}");
    assert_eq!(
        read(&dir, "a.py"),
        UNFORMATTED,
        "the walk still prunes excluded files, got:\n{text}"
    );
}
