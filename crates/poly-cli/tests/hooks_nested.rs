//! A `[hooks]` table in a nested `poly.toml` runs on commit, confined to its own
//! directory: git runs hooks from the repository root, so the root `poly.toml`
//! alone used to be consulted.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn write(repo: &Path, name: &str, contents: &str) {
    let path = repo.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write file");
}

fn run_pre_commit(repo: &Path) -> Output {
    Command::new(POLY)
        .args(["hooks", "run", "pre-commit"])
        .current_dir(repo)
        .output()
        .expect("poly invocation")
}

const ROOT_CONFIG: &str = r#"
[hooks]
isolate = false

[hooks.pre-commit.commands.root-job]
run = "printf root > root-ran.txt"
"#;

const NESTED_CONFIG: &str = r#"
[hooks.pre-commit.commands.nested-job]
run = "printf ran > ran.txt"
"#;

fn stage(repo: &Path, name: &str) {
    write(repo, name, "x\n");
    git(repo, &["add", name]);
}

#[test]
fn a_nested_hook_runs_in_its_directory_when_a_file_under_it_is_staged() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", ROOT_CONFIG);
    write(root, "svc/poly.toml", NESTED_CONFIG);
    stage(root, "svc/a.txt");

    let output = run_pre_commit(root);
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{report}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.contains("svc:nested-job"), "{report}");
    assert!(
        root.join("svc/ran.txt").is_file(),
        "the nested job runs with svc as its cwd"
    );
    assert!(!root.join("ran.txt").exists());
    assert!(root.join("root-ran.txt").is_file(), "the root hooks still run");
}

#[test]
fn a_nested_hook_does_not_run_for_files_outside_its_directory() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", ROOT_CONFIG);
    write(root, "svc/poly.toml", NESTED_CONFIG);
    stage(root, "other/a.txt");

    let output = run_pre_commit(root);
    assert!(output.status.success());
    assert!(!root.join("svc/ran.txt").exists());
    assert!(root.join("root-ran.txt").is_file());
}

#[test]
fn a_nested_job_glob_is_relative_to_its_directory_and_files_are_passed_relative_to_it() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", ROOT_CONFIG);
    write(
        root,
        "svc/poly.toml",
        r#"
[hooks.pre-commit.commands.src-only]
files = "src/*.txt"
run = '''sh -c 'for f; do echo "$f"; done > args.txt' --'''
"#,
    );
    stage(root, "svc/lib/skipped.txt");
    let skipped = run_pre_commit(root);
    assert!(skipped.status.success());
    assert!(
        !root.join("svc/args.txt").exists(),
        "svc/lib/skipped.txt is not under svc/src"
    );

    stage(root, "svc/src/used.txt");
    let ran = run_pre_commit(root);
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    assert_eq!(
        std::fs::read_to_string(root.join("svc/args.txt")).unwrap(),
        "src/used.txt\n"
    );
}

#[test]
fn a_broken_nested_config_stops_the_run() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", ROOT_CONFIG);
    write(root, "svc/poly.toml", "[hooks\n");
    stage(root, "svc/a.txt");

    let output = run_pre_commit(root);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("svc/poly.toml"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn installing_binds_the_git_hooks_a_nested_config_uses() {
    let repo = init_repo();
    let root = repo.path();
    write(root, "poly.toml", ROOT_CONFIG);
    write(
        root,
        "svc/poly.toml",
        "[hooks.pre-push.commands.slow]\nrun = \"true\"\n",
    );
    stage(root, "svc/a.txt");
    git(root, &["add", "poly.toml", "svc/poly.toml"]);

    let output = Command::new(POLY)
        .args(["hooks", "install"])
        .current_dir(root)
        .output()
        .expect("poly invocation");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        root.join(".git/hooks/pre-push").is_file(),
        "the nested pre-push table binds its shim"
    );
}
