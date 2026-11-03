//! `poly commit` end to end: the `[commit]` table of `poly.toml`, flag overrides, exit codes
//! and the rewrite-in-place flow, run against a tempdir so no ambient config is discovered.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn poly_commit(dir: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("commit")
        .args(args)
        .current_dir(dir)
        .env("POLY_OFFLINE", "1")
        .output()
        .expect("run poly commit")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn conventional_message_passes_silently() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &["--message", "feat: add login"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(output.stderr.is_empty(), "{}", stderr(&output));
}

#[test]
fn non_conventional_message_fails_with_gitfluff_prefix() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &["--message", "nope: not a type"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("gitfluff: error: "), "{}", stderr(&output));
}

#[test]
fn poly_toml_commit_rules_apply() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("poly.toml"),
        "[commit.rules]\nno_emojis = true\n[[commit.rules.excludes]]\npattern = \"^feat: wip\"\nmessage = \"no wip\"\n",
    )
    .unwrap();

    let wip = poly_commit(dir.path(), &["--message", "feat: wip thing"]);
    assert_eq!(wip.status.code(), Some(1));
    assert!(stderr(&wip).contains("no wip"), "{}", stderr(&wip));

    let emoji = poly_commit(dir.path(), &["--message", "feat: add \u{1F680} launch"]);
    assert_eq!(emoji.status.code(), Some(1), "{}", stderr(&emoji));
}

#[test]
fn write_strips_ai_trailer_from_the_message_file() {
    let dir = tempdir().unwrap();
    let msg = dir.path().join("MSG");
    fs::write(
        &msg,
        "feat: add login\n\nbody\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n",
    )
    .unwrap();

    let output = poly_commit(dir.path(), &["--write", "MSG"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(&msg).unwrap(), "feat: add login\n\nbody\n");
}

#[test]
fn missing_message_source_is_an_error() {
    let dir = tempdir().unwrap();
    let output = poly_commit(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("no commit message source"));
}
