//! `--quiet`: the level below the default for `pretty` output.
//!
//! poly's verbosity only ever went up. A run over a large repository printed its
//! headline and then buried it under discovery and skip detail, so the last
//! thing on screen was a wall of "poly declined to check this" — the opposite of
//! what the run actually achieved. `--quiet` removes the *per-file detail* and
//! nothing else.
//!
//! The load-bearing constraint these tests pin down: the summary line is
//! **byte-identical** with and without `--quiet`. poly's honesty guarantee is
//! that a skip is visible, and the summary already carries every count and
//! reason. A `--quiet` that hid those would turn the guarantee off.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A `.csproj` is XML-ish but no poly engine claims the extension, so naming it
/// explicitly produces a skip — the note `--quiet` must suppress.
const CSPROJ: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n  </PropertyGroup>\n</Project>\n";

/// A repo that produces both a skip note and real findings.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).expect("write fixture");
    write("App.csproj", CSPROJ);
    write("a.py", "x = 1\n");
    write("b.py", "y = 2\n");
    write("c.json", "{ \"a\": 1 }\n");
    write("d.md", "# Title\n");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run poly")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The summary block: the headline and every breakdown line under it, which is
/// everything from the last unindented line to the end of the report.
fn summary_block(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .rposition(|line| !line.is_empty() && !line.starts_with(' '))
        .unwrap_or_else(|| panic!("no summary block in:\n{text}"));
    lines[start..].join("\n")
}

/// The headline test. Skip detail is present by default and gone under
/// `--quiet`, while the summary line — counts, reasons and all — does not move
/// by a single byte.
#[test]
fn quiet_suppresses_skip_detail_and_keeps_the_summary_line_byte_identical() {
    let dir = repo();
    let args = ["lint", "--no-workspace", "App.csproj", "a.py", "b.py", "c.json", "d.md"];

    let loud = poly(dir.path(), &args);
    let hushed = poly(dir.path(), &[&args[..], &["--quiet"]].concat());

    let loud_text = combined(&loud);
    let hushed_text = combined(&hushed);
    let loud_summary = summary_block(&stdout(&loud));
    let hushed_summary = summary_block(&stdout(&hushed));

    assert!(
        loud_text.contains("skipped App.csproj"),
        "the default must name the skipped file, got:\n{loud_text}"
    );
    assert!(
        !hushed_text.contains("skipped App.csproj"),
        "--quiet must suppress the per-file skip note, got:\n{hushed_text}"
    );
    assert_eq!(
        loud_summary, hushed_summary,
        "--quiet must not change the summary block: counts and reasons stay visible"
    );
    assert!(
        hushed_summary.contains("1 file skipped: no matching engine"),
        "the summary must still report the skip count and reason under --quiet, got:\n{hushed_summary}"
    );
    assert!(
        hushed_summary.contains("4 files linted"),
        "the summary must still report what was linted under --quiet, got:\n{hushed_summary}"
    );
    assert_eq!(
        loud.status.code(),
        hushed.status.code(),
        "--quiet must not change the exit code"
    );
}

/// `poly fmt` shares the flag, and shares the constraint.
#[test]
fn quiet_suppresses_skip_detail_for_fmt_and_keeps_the_summary() {
    let dir = repo();
    let args = ["fmt", "--check", "App.csproj", "a.py"];

    let loud = combined(&poly(dir.path(), &args));
    let hushed = combined(&poly(dir.path(), &[&args[..], &["--quiet"]].concat()));

    assert!(
        loud.contains("skipped App.csproj"),
        "the default must name the skipped file, got:\n{loud}"
    );
    assert!(
        !hushed.contains("skipped App.csproj"),
        "--quiet must suppress the per-file skip note, got:\n{hushed}"
    );
}

/// Findings still print: `--quiet` is not `--silent`.
#[test]
fn quiet_still_prints_findings() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.py"), "import os\n").expect("write");

    let output = poly(dir.path(), &["lint", "--no-workspace", "--quiet", "a.py"]);
    let text = combined(&output);

    assert!(
        text.contains("a.py"),
        "--quiet must still print the finding lines, got:\n{text}"
    );
}

/// Asking for less and more at once is a user error worth catching at parse
/// time, the way `--force-exclude` / `--include-excluded` already is.
#[test]
fn quiet_conflicts_with_verbose_and_debug() {
    let dir = repo();
    for (subcommand, other) in [
        ("lint", "--verbose"),
        ("lint", "--debug"),
        ("fmt", "--verbose"),
        ("fmt", "--debug"),
    ] {
        let output = poly(dir.path(), &[subcommand, "--quiet", other, "."]);
        let text = combined(&output);
        assert_eq!(
            output.status.code(),
            Some(2),
            "poly {subcommand} --quiet {other} must be rejected by clap, got:\n{text}"
        );
        assert!(
            text.contains("cannot be used with"),
            "the rejection must name the conflict, got:\n{text}"
        );
    }
}

/// `--format json` is a machine contract with its own payload. `--quiet` is a
/// `pretty`-only control and must not move a byte of it.
#[test]
fn quiet_does_not_change_json_output() {
    let dir = repo();
    let args = ["lint", "--no-workspace", "--format", "json", "."];

    let loud = poly(dir.path(), &args);
    let hushed = poly(dir.path(), &[&args[..], &["--quiet"]].concat());

    assert_eq!(
        stdout(&loud),
        stdout(&hushed),
        "--quiet must leave the json document byte-identical"
    );
    assert_eq!(loud.status.code(), hushed.status.code(), "exit code must not move");
}

/// The same for `toon`.
#[test]
fn quiet_does_not_change_toon_output() {
    let dir = repo();
    let args = ["fmt", "--check", "--format", "toon", "."];

    let loud = poly(dir.path(), &args);
    let hushed = poly(dir.path(), &[&args[..], &["--quiet"]].concat());

    assert_eq!(
        stdout(&loud),
        stdout(&hushed),
        "--quiet must leave the toon document byte-identical"
    );
}
