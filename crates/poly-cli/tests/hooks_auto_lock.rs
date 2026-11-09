//! `poly hooks install` records the `poly.lock` entries it is missing (issue
//! #158), `--locked` / `POLY_LOCKED` keep the strict failure, and the hook run
//! itself never writes the lock.
//!
//! Hook sources are local Git repositories addressed by path, so nothing here
//! touches the network. Gated to Unix like `hooks_e2e.rs`: the catalog's
//! `check` is POSIX shell.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=Poly Test", "-c", "user.email=poly@example.invalid"])
        .args(["-c", "commit.gpgSign=false"])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A producer repository with one committed catalog; returns it and its HEAD.
fn producer() -> (TempDir, String) {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(
        dir.path().join("poly-hooks.toml"),
        r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
[[hooks.paths]]
channel = "shell"
check = "command -v true"
run = "true"
"#,
    )
    .expect("write catalog");
    git(dir.path(), &["add", "poly-hooks.toml"]);
    git(dir.path(), &["commit", "-qm", "catalog"]);
    let head = git(dir.path(), &["rev-parse", "HEAD"]);
    (dir, head)
}

fn source_entry(id: &str, repository: &Path) -> String {
    format!(
        "[[hooks.sources]]\nid = {id:?}\ngit = {:?}\nrevision = \"HEAD\"\nhooks = [\"validate\"]\n",
        repository.to_string_lossy()
    )
}

/// A consumer repository whose `poly.toml` selects `sources`.
fn consumer(sources: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("poly.toml"), sources).expect("write poly.toml");
    std::fs::write(
        dir.path().join("poly.local.toml"),
        "[hook_preferences]\nchannels = [\"shell\"]\n",
    )
    .expect("write poly.local.toml");
    dir
}

fn poly(repo: &Path, cache: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(POLY);
    command
        .args(args)
        .current_dir(repo)
        .env("POLY_CACHE_HOME", cache)
        .env_remove("POLY_OFFLINE")
        .env_remove("POLY_LOCKED");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("poly invocation")
}

fn lock_text(repo: &Path) -> Option<String> {
    std::fs::read_to_string(repo.join("poly.lock")).ok()
}

#[test]
fn install_locks_a_missing_hook_source() {
    let (producer, head) = producer();
    let repo = consumer(&source_entry("rules", producer.path()));
    let cache = TempDir::new().expect("cache");

    let output = poly(repo.path(), cache.path(), &["hooks", "install"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "install must succeed: {stderr}");
    assert!(
        stderr.contains("poly hooks install: locked hook source `rules`") && stderr.contains(&head),
        "one note names the entry added: {stderr}"
    );
    let lock = lock_text(repo.path()).expect("poly.lock written");
    assert!(lock.contains("id = \"rules\"") && lock.contains(&head), "{lock}");
    assert!(
        repo.path().join(".git/hooks/pre-commit").is_file(),
        "the shim is installed"
    );
}

#[test]
fn locked_keeps_the_strict_error_and_writes_nothing() {
    let (producer, _) = producer();
    let repo = consumer(&source_entry("rules", producer.path()));
    let cache = TempDir::new().expect("cache");

    for (args, env) in [
        (vec!["hooks", "install", "--locked"], vec![]),
        (vec!["hooks", "install"], vec![("POLY_LOCKED", "1")]),
    ] {
        let output = poly(repo.path(), cache.path(), &args, &env);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?} {env:?}: {stderr}");
        assert!(stderr.contains("has no lock entry"), "{args:?} {env:?}: {stderr}");
        assert_eq!(
            lock_text(repo.path()),
            None,
            "{args:?} {env:?} must not write poly.lock"
        );
        assert!(!repo.path().join(".git/hooks/pre-commit").exists());
    }
}

#[test]
fn install_keeps_existing_pins_and_adds_only_the_missing_one() {
    let (first, first_head) = producer();
    let repo = consumer(&source_entry("first", first.path()));
    let cache = TempDir::new().expect("cache");
    let output = poly(repo.path(), cache.path(), &["hooks", "install"], &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let initial = lock_text(repo.path()).expect("poly.lock written");

    // A complete lock is left byte for byte, and no note is printed.
    let output = poly(repo.path(), cache.path(), &["hooks", "install"], &[]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("locked hook source"));
    assert_eq!(lock_text(repo.path()).as_deref(), Some(initial.as_str()));

    // Move `first` past its pin, then declare a second source.
    std::fs::write(first.path().join("later.txt"), "later").unwrap();
    git(first.path(), &["add", "later.txt"]);
    git(first.path(), &["commit", "-qm", "later"]);
    let first_new_head = git(first.path(), &["rev-parse", "HEAD"]);
    assert_ne!(first_new_head, first_head);
    let (second, second_head) = producer();
    std::fs::write(
        repo.path().join("poly.toml"),
        format!(
            "{}\n{}",
            source_entry("first", first.path()),
            source_entry("second", second.path())
        ),
    )
    .unwrap();

    let output = poly(repo.path(), cache.path(), &["hooks", "install"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("locked hook source `second`"), "{stderr}");
    assert!(!stderr.contains("locked hook source `first`"), "{stderr}");
    let lock = lock_text(repo.path()).unwrap();
    assert!(lock.contains(&first_head), "the existing pin is kept: {lock}");
    assert!(
        !lock.contains(&first_new_head),
        "the existing pin is never upgraded: {lock}"
    );
    assert!(lock.contains(&second_head), "{lock}");
}

/// The hook run stays strict: it fires inside `git commit`, where rewriting a
/// tracked file would leave the commit without it.
#[test]
fn hook_run_never_writes_the_lock() {
    let (producer, _) = producer();
    let repo = consumer(&source_entry("rules", producer.path()));
    let cache = TempDir::new().expect("cache");

    let output = poly(repo.path(), cache.path(), &["hooks", "run", "pre-commit"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("has no lock entry"), "{stderr}");
    assert_eq!(lock_text(repo.path()), None);
}
