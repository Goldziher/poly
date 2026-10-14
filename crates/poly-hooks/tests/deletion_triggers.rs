//! Deleted paths trigger repository checks without becoming per-file inputs. ~keep
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use poly_hooks::filter::FilePattern;
use poly_hooks::model::{HookStatus, SkipReason};
use poly_hooks::{Hook, HookRunRequest, StageSpec, run};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn deletion_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("removed.toml"), "version = 1\n").unwrap();
    git(root, &["add", "removed.toml"]);
    git(root, &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "initial"]);
    git(root, &["rm", "-q", "removed.toml"]);
    repo
}

fn assert_deletion_triggers(root: &Path, files: Vec<PathBuf>, snapshot: bool) {
    assert_eq!(files, vec![PathBuf::from("removed.toml")]);
    let mut workspace = Hook::run("workspace", "test ! -e removed.toml && printf verified");
    workspace.files = Some(FilePattern::glob(vec!["*.toml".into()]).unwrap());
    workspace.pass_filenames = false;
    workspace.workspace = true;
    let mut per_file = Hook::run("per-file", "exit 1");
    per_file.files.clone_from(&workspace.files);
    let snapshot_dir = TempDir::new().unwrap();
    let request = HookRunRequest {
        root: root.to_path_buf(),
        work_root: snapshot.then(|| snapshot_dir.path().to_path_buf()),
        files,
        stages: vec![StageSpec {
            hooks: vec![workspace, per_file],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    };
    let outcome = run(request).unwrap();
    assert!(outcome.success());
    assert_eq!(outcome.stages[0].hooks.len(), 2);
    assert_eq!(outcome.stages[0].hooks[0].status, HookStatus::Passed);
    assert_eq!(outcome.stages[0].hooks[0].output, b"verified");
    assert_eq!(
        outcome.stages[0].hooks[1].status,
        HookStatus::Skipped(SkipReason::NoFiles)
    );
}

#[test]
fn staged_deletion_triggers_workspace_hook_and_skips_per_file_hook() {
    let repo = deletion_repo();
    let root = repo.path();
    std::fs::write(root.join("removed.toml"), "unstaged recreation\n").unwrap();
    assert_deletion_triggers(root, poly_hooks::git::get_staged_files(root).unwrap(), true);
}

#[test]
fn committed_deletion_triggers_workspace_hook_and_skips_per_file_hook() {
    let repo = deletion_repo();
    let root = repo.path();
    let old = git(root, &["rev-parse", "HEAD"]);
    git(root, &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "delete"]);
    let new = git(root, &["rev-parse", "HEAD"]);
    assert_deletion_triggers(
        root,
        poly_hooks::git::get_changed_files(&old, &new, root).unwrap(),
        false,
    );
}

#[test]
fn mixed_changes_pass_only_live_paths_to_per_file_hooks() {
    let repo = deletion_repo();
    let root = repo.path();
    std::fs::write(root.join("live.toml"), "version = 2\n").unwrap();
    git(root, &["add", "live.toml"]);
    let files = poly_hooks::git::get_staged_files(root).unwrap();
    assert_eq!(files, vec![PathBuf::from("live.toml"), PathBuf::from("removed.toml")]);
    let mut per_file = Hook::run("per-file", "printf '%s'");
    per_file.files = Some(FilePattern::glob(vec!["*.toml".into()]).unwrap());
    let mut scoped = Hook::run("scoped", "printf checked");
    scoped.pass_filenames = false;
    scoped.files = Some(FilePattern::glob(vec!["removed.toml".into()]).unwrap());
    let outcome = run(HookRunRequest {
        root: root.to_path_buf(),
        files,
        stages: vec![StageSpec {
            hooks: vec![per_file, scoped],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    })
    .unwrap();
    assert!(outcome.success());
    assert_eq!(outcome.stages[0].hooks.len(), 2);
    assert_eq!(outcome.stages[0].hooks[0].output, b"live.toml");
    assert_eq!(outcome.stages[0].hooks[1].output, b"checked");
}

#[test]
fn dangling_symlink_is_still_a_per_file_hook_input() {
    let repo = deletion_repo();
    let root = repo.path();
    std::os::unix::fs::symlink("absent-target", root.join("dangling.toml")).unwrap();
    git(root, &["add", "dangling.toml"]);
    let files = poly_hooks::git::get_staged_files(root).unwrap();
    assert_eq!(
        files,
        vec![PathBuf::from("dangling.toml"), PathBuf::from("removed.toml")]
    );
    let outcome = run(HookRunRequest {
        root: root.to_path_buf(),
        files,
        stages: vec![StageSpec {
            hooks: vec![Hook::run("per-file", "printf '%s'")],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    })
    .unwrap();
    assert!(outcome.success());
    assert_eq!(outcome.stages[0].hooks.len(), 1);
    assert_eq!(outcome.stages[0].hooks[0].output, b"dangling.toml");
}
