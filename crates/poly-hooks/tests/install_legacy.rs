//! What `install` does with a hook that was already there, and that the
//! installed shim actually runs it.

#![cfg(unix)]

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use poly_hooks::HookType;
use poly_hooks::install::{InstallOptions, Preserved, install_with};
use tempfile::TempDir;

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_foreign_hook_is_kept_and_reported_as_chained() {
    let hooks = TempDir::new().unwrap();
    script(&hooks.path().join("pre-commit"), "echo mine\n");

    let report = install_with(hooks.path(), &[HookType::PreCommit], InstallOptions::default()).unwrap();

    assert_eq!(
        report.preserved,
        vec![Preserved::Chained(hooks.path().join("pre-commit.legacy"))]
    );
}

#[test]
fn a_lefthook_hook_is_kept_but_reported_as_not_run() {
    let hooks = TempDir::new().unwrap();
    script(
        &hooks.path().join("pre-commit"),
        "call_lefthook run pre-commit\n# LEFTHOOK\n",
    );

    let report = install_with(hooks.path(), &[HookType::PreCommit], InstallOptions::default()).unwrap();

    assert_eq!(
        report.preserved,
        vec![Preserved::LefthookKept(hooks.path().join("pre-commit.legacy"))]
    );
}

#[test]
fn replace_lefthook_discards_the_lefthook_hook() {
    let hooks = TempDir::new().unwrap();
    script(&hooks.path().join("pre-commit"), "call_lefthook run pre-commit\n");

    let options = InstallOptions {
        replace_lefthook: true,
        ..InstallOptions::default()
    };
    let report = install_with(hooks.path(), &[HookType::PreCommit], options).unwrap();

    assert_eq!(
        report.preserved,
        vec![Preserved::LefthookReplaced(hooks.path().join("pre-commit"))]
    );
    assert!(!hooks.path().join("pre-commit.legacy").exists());
}

#[test]
fn replace_lefthook_still_preserves_a_foreign_hook() {
    let hooks = TempDir::new().unwrap();
    script(&hooks.path().join("pre-commit"), "echo mine\n");

    let options = InstallOptions {
        replace_lefthook: true,
        ..InstallOptions::default()
    };
    install_with(hooks.path(), &[HookType::PreCommit], options).unwrap();

    assert!(hooks.path().join("pre-commit.legacy").exists());
}

/// Run the installed `pre-push` shim with a fake `poly` on `PATH`, returning the
/// log both programs append to and the shim's exit code.
fn run_shim(hooks: &Path, bin: &Path, log: &Path, stdin: &str) -> i32 {
    let mut child = Command::new(hooks.join("pre-push"))
        .args(["origin", "url"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LOG", log)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait().unwrap().code().unwrap()
}

fn fake_poly(bin: &Path) {
    std::fs::create_dir_all(bin).unwrap();
    script(&bin.join("poly"), "echo \"poly $* stdin=$(cat)\" >> \"$LOG\"\n");
}

#[test]
fn the_shim_runs_the_kept_hook_before_poly_with_args_and_stdin() {
    let dir = TempDir::new().unwrap();
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    script(&hooks.join("pre-push"), "echo \"legacy $* stdin=$(cat)\" >> \"$LOG\"\n");
    install_with(&hooks, &[HookType::PrePush], InstallOptions::default()).unwrap();
    let bin = dir.path().join("bin");
    fake_poly(&bin);
    let log = dir.path().join("log");

    let code = run_shim(&hooks, &bin, &log, "refs/heads/main abc\n");

    assert_eq!(code, 0);
    let lines = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        lines,
        "legacy origin url stdin=refs/heads/main abc\n\
         poly hooks hook-impl --hook-type=pre-push -- origin url stdin=refs/heads/main abc\n"
    );
}

#[test]
fn a_failing_kept_hook_blocks_poly() {
    let dir = TempDir::new().unwrap();
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    script(&hooks.join("pre-push"), "exit 7\n");
    install_with(&hooks, &[HookType::PrePush], InstallOptions::default()).unwrap();
    let bin = dir.path().join("bin");
    fake_poly(&bin);
    let log = dir.path().join("log");

    let code = run_shim(&hooks, &bin, &log, "");

    assert_eq!(code, 7);
    assert!(!log.exists(), "poly must not run after a failing legacy hook");
}

#[test]
fn the_shim_never_runs_a_kept_lefthook_hook() {
    let dir = TempDir::new().unwrap();
    let hooks = dir.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    script(&hooks.join("pre-push"), "echo lefthook-ran >> \"$LOG\"\n# lefthook\n");
    install_with(&hooks, &[HookType::PrePush], InstallOptions::default()).unwrap();
    let bin = dir.path().join("bin");
    fake_poly(&bin);
    let log = dir.path().join("log");

    run_shim(&hooks, &bin, &log, "");

    let lines = std::fs::read_to_string(&log).unwrap();
    assert!(!lines.contains("lefthook-ran"), "{lines}");
    assert!(lines.starts_with("poly hooks hook-impl"), "{lines}");
}
