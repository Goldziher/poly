//! Staged-scope contract for the commit gate.
//!
//! When a run carries a staged snapshot ([`HookRunRequest::work_root`]) every
//! hook — per-file and whole-workspace alike — must be evaluated against the
//! **staged** bytes, because those are the bytes the commit will capture. A
//! per-file hook that judged the worktree instead could pass a commit whose
//! staged content it never saw (and fail one whose staged content was fine),
//! which is the defect these tests pin down.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use poly_hooks::model::{HookStatus, ValidatedTree};
use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
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
    output
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    dir
}

/// Materialize the repository's index into `dest` — the same `git
/// checkout-index` the real [`poly_hooks::snapshot::StagedSnapshot`] uses,
/// inlined so the test stays hermetic instead of writing to the per-user cache.
fn snapshot_of(repo: &Path, dest: &Path) {
    let prefix = format!("--prefix={}/", dest.display());
    git(repo, &["checkout-index", "-f", "-a", &prefix]);
}

/// A hook that fails when any file it is given contains `BROKEN` — a stand-in
/// for any content-validating per-file check.
fn reject_broken(id: &str) -> Hook {
    Hook::run(id, "! grep -l BROKEN")
}

fn isolated_request(root: &Path, snapshot: &Path, hooks: Vec<Hook>, files: &[&str]) -> HookRunRequest {
    HookRunRequest {
        root: root.to_path_buf(),
        work_root: Some(snapshot.to_path_buf()),
        files: files.iter().map(PathBuf::from).collect(),
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks,
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    }
}

fn staged_blob(repo: &Path, path: &str) -> String {
    let output = git(repo, &["show", &format!(":{path}")]);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// THE FALSE PASS. The index holds invalid content while the worktree copy was
/// fixed afterwards; a gate that reads the worktree passes a commit it never
/// checked.
#[test]
fn per_file_hook_rejects_invalid_staged_content_even_when_the_worktree_is_clean() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("data.txt"), "this line is BROKEN\n").unwrap();
    git(root, &["add", "data.txt"]);
    // …and then the author "fixed" only the worktree copy.
    std::fs::write(root.join("data.txt"), "this line is fine\n").unwrap();

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let outcome = run(isolated_request(
        root,
        snapshot.path(),
        vec![reject_broken("no-broken")],
        &["data.txt"],
    ))
    .expect("run");

    assert!(
        !outcome.success(),
        "the staged bytes are invalid, so the gate must fail regardless of the worktree"
    );
}

/// The mirror image: an unstaged edit must not fail a commit whose staged
/// content is fine. A gate that blocks on bytes that are not being committed is
/// just as wrong as one that passes on them.
#[test]
fn per_file_hook_ignores_an_unstaged_worktree_edit() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("data.txt"), "this line is fine\n").unwrap();
    git(root, &["add", "data.txt"]);
    std::fs::write(root.join("data.txt"), "BROKEN only in the worktree\n").unwrap();

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let outcome = run(isolated_request(
        root,
        snapshot.path(),
        vec![reject_broken("no-broken")],
        &["data.txt"],
    ))
    .expect("run");

    assert!(outcome.success(), "an unstaged edit must not fail the commit gate");
}

/// The `git add -p` case: one hunk staged, another left dirty. The staged
/// version is the truth, so the bad unstaged hunk is invisible to the gate.
#[test]
fn partially_staged_file_is_judged_by_its_staged_version_only() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("data.txt"), "alpha\nbeta\n").unwrap();
    git(root, &["add", "data.txt"]);
    git(root, &["commit", "-qm", "init"]);

    // Stage a good edit, then add a bad one on top without staging it — the
    // shape `git add -p` leaves behind.
    std::fs::write(root.join("data.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git(root, &["add", "data.txt"]);
    std::fs::write(root.join("data.txt"), "alpha\nbeta\ngamma\nBROKEN\n").unwrap();

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let outcome = run(isolated_request(
        root,
        snapshot.path(),
        vec![reject_broken("no-broken")],
        &["data.txt"],
    ))
    .expect("run");

    assert!(
        outcome.success(),
        "only the staged hunks are being committed, and they are valid"
    );
}

/// A `stage_fixed` hook whose fix lands on a file with no unstaged edits: the
/// fix is copied into the worktree and staged, so the commit proceeds with the
/// fixed bytes. Nothing can be lost — the worktree copy was identical to the
/// index.
#[test]
fn stage_fixed_write_back_reaches_the_worktree_and_the_index_when_nothing_is_unstaged() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "unformatted\n").unwrap();
    git(root, &["add", "f.txt"]);

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let mut hook = Hook::run("fixer", "echo formatted > f.txt");
    hook.pass_filenames = false;
    hook.stage_fixed = true;

    let outcome = run(isolated_request(root, snapshot.path(), vec![hook], &["f.txt"])).expect("run");

    assert!(
        outcome.success(),
        "a stage_fixed hook that fixed its input still passes"
    );
    assert!(outcome.stages[0].hooks[0].files_modified);
    assert_eq!(staged_blob(root, "f.txt"), "formatted\n", "the fix must be staged");
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "formatted\n",
        "the fix must also land in the worktree, so index and worktree stay in sync"
    );
}

/// The dangerous case: the fix was computed from staged bytes, but the worktree
/// copy carries unstaged work. Writing the fix there would destroy it, so the
/// fix is withheld and the commit is blocked for the author to resolve.
#[test]
fn stage_fixed_write_back_is_withheld_when_the_worktree_copy_has_unstaged_edits() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "unformatted\n").unwrap();
    git(root, &["add", "f.txt"]);
    std::fs::write(root.join("f.txt"), "unformatted\nprecious unstaged work\n").unwrap();

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let mut hook = Hook::run("fixer", "echo formatted > f.txt");
    hook.pass_filenames = false;
    hook.stage_fixed = true;

    let outcome = run(isolated_request(root, snapshot.path(), vec![hook], &["f.txt"])).expect("run");

    assert!(!outcome.success(), "the commit must be blocked rather than guessed at");
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "unformatted\nprecious unstaged work\n",
        "unstaged work must never be overwritten by a fix derived from staged bytes"
    );
    assert_eq!(
        staged_blob(root, "f.txt"),
        "unformatted\n",
        "nothing may be staged behind the author's back"
    );
    let report = poly_hooks::HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("f.txt"),
        "the report must name the file whose fix was withheld: {report}"
    );
}

/// A hook *without* `stage_fixed` never promised to change what gets committed,
/// but its rewrite must still reach the author's working tree — otherwise
/// isolation would silently swallow a fix that a worktree run would have handed
/// them, and the snapshot's next refresh would erase it.
#[test]
fn a_rewrite_without_stage_fixed_still_reaches_the_worktree_unstaged() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "unformatted\n").unwrap();
    git(root, &["add", "f.txt"]);

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let mut hook = Hook::run("fixer", "echo formatted > f.txt");
    hook.pass_filenames = false;
    hook.stage_fixed = false;

    let outcome = run(isolated_request(root, snapshot.path(), vec![hook], &["f.txt"])).expect("run");

    assert!(outcome.success());
    assert!(!outcome.stages[0].hooks[0].files_modified, "nothing was staged");
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "formatted\n",
        "the rewrite must reach the worktree the author is looking at"
    );
    assert_eq!(
        staged_blob(root, "f.txt"),
        "unformatted\n",
        "without stage_fixed the index is left alone"
    );
}

/// Silence about which bytes were checked is the underlying defect, so every
/// hook records the tree it was evaluated against and the report says so.
#[test]
fn every_hook_records_and_reports_the_tree_it_validated() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("data.txt"), "fine\n").unwrap();
    git(root, &["add", "data.txt"]);

    let snapshot = TempDir::new().expect("snapshot dir");
    snapshot_of(root, snapshot.path());

    let mut workspace_hook = Hook::run("ws", "true");
    workspace_hook.workspace = true;
    workspace_hook.always_run = true;
    workspace_hook.pass_filenames = false;

    let outcome = run(isolated_request(
        root,
        snapshot.path(),
        vec![reject_broken("per-file"), workspace_hook],
        &["data.txt"],
    ))
    .expect("run");

    assert!(outcome.success());
    for hook in &outcome.stages[0].hooks {
        assert_eq!(
            hook.validated,
            ValidatedTree::StagedIndex,
            "hook `{}` must report the staged tree",
            hook.id
        );
    }
    let report = poly_hooks::HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("staged"),
        "the report must name the tree that was validated: {report}"
    );
}

/// A run without a snapshot (`--all-files`, a manual `poly hooks run`) is about
/// the worktree, and must keep saying so — both in behaviour and in the record.
#[test]
fn a_run_without_a_snapshot_still_validates_the_worktree() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("data.txt"), "fine\n").unwrap();
    git(root, &["add", "data.txt"]);
    std::fs::write(root.join("data.txt"), "BROKEN in the worktree\n").unwrap();

    let request = HookRunRequest {
        root: root.to_path_buf(),
        files: vec![PathBuf::from("data.txt")],
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks: vec![reject_broken("no-broken")],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    };
    let outcome = run(request).expect("run");

    assert!(
        matches!(outcome.stages[0].hooks[0].status, HookStatus::Failed { .. }),
        "a worktree run judges the worktree"
    );
    assert_eq!(outcome.stages[0].hooks[0].validated, ValidatedTree::Worktree);
}
