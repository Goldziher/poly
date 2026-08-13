//! What may be written into the author's working tree.
//!
//! Under a staged run a hook rewrites the **snapshot**, and the runner then
//! carries that fix back across into the worktree. That write is the only place
//! poly touches a file nobody asked it to format, and it is the one place in the
//! hook runner where a wrong answer destroys work that cannot be recovered. Two
//! separate defects these tests pin down:
//!
//! - the gate used to be `git diff-files`, whose stat-based answer calls a
//!   genuinely-modified file *clean* whenever the index stat cache is stale or
//!   suppressed — and the fix was then copied over the author's unstaged edit;
//! - the destination was opened with `fs::copy`, which **follows symlinks**, so
//!   a tracked symlink whose blob is an absolute path outside the repository
//!   turned every fixer into an arbitrary file write.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use poly_hooks::model::{HookStatus, WithheldFix, WithheldReason};
use poly_hooks::snapshot::StagedSnapshot;
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

fn staged_blob(repo: &Path, path: &str) -> String {
    let output = git(repo, &["show", &format!(":{path}")]);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A hook that overwrites `file` with `content`, as a formatter would.
fn fixer(file: &str, content: &str) -> Hook {
    let mut hook = Hook::run("fixer", format!("printf '{content}' > {file}"));
    hook.pass_filenames = false;
    hook.stage_fixed = true;
    hook
}

fn run_isolated(root: &Path, snapshot: &Path, hook: Hook, files: &[&str]) -> poly_hooks::model::HookRunOutcome {
    run(HookRunRequest {
        root: root.to_path_buf(),
        work_root: Some(snapshot.to_path_buf()),
        files: files.iter().map(PathBuf::from).collect(),
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks: vec![hook],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    })
    .expect("run")
}

fn report_of(outcome: &poly_hooks::model::HookRunOutcome) -> String {
    poly_hooks::HookRunReporter::new().render(outcome)
}

/// THE DATA LOSS. `git diff-files` is stat-based, so `--assume-unchanged` (like
/// a stale stat cache, a sparse checkout, `--skip-worktree`, coarse mtime, or a
/// network filesystem) makes it report a file with real unstaged edits as
/// clean. A write-back gated on that answer copies the staged-derived fix over
/// the author's work and destroys it silently.
#[test]
fn write_back_is_withheld_when_the_stat_cache_calls_a_modified_file_clean() {
    let repo = init_repo();
    let cache = TempDir::new().expect("cache home");
    let root = repo.path();
    std::fs::write(root.join("f.txt"), "unformatted\n").unwrap();
    git(root, &["add", "f.txt"]);
    std::fs::write(root.join("f.txt"), "unformatted\nprecious unstaged work\n").unwrap();
    git(root, &["update-index", "--assume-unchanged", "f.txt"]);

    let stat_cache_says_clean = Command::new("git")
        .args(["diff-files", "--quiet", "--", "f.txt"])
        .current_dir(root)
        .status()
        .expect("git diff-files")
        .success();
    assert!(
        stat_cache_says_clean,
        "precondition: the primitive this gate used to trust reports this file clean"
    );

    let snapshot = StagedSnapshot::create_in(cache.path(), root).expect("snapshot");
    let outcome = run_isolated(root, snapshot.path(), fixer("f.txt", "formatted\\n"), &["f.txt"]);

    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "unformatted\nprecious unstaged work\n",
        "unstaged work must survive a lying stat cache"
    );
    assert_eq!(
        staged_blob(root, "f.txt"),
        "unformatted\n",
        "nothing may be staged behind the author's back"
    );
    assert!(!outcome.success(), "the commit must be blocked rather than guessed at");
    assert_eq!(
        outcome.stages[0].hooks[0].status,
        HookStatus::FixWithheld(vec![WithheldFix::new("f.txt", WithheldReason::UnstagedChanges)]),
        "the withheld fix must be reported with the reason that actually applies"
    );
    let report = report_of(&outcome);
    assert!(
        report.contains("        f.txt — the worktree copy has unstaged changes the fix never saw\n"),
        "the report must name the file and say why: {report}"
    );
    assert!(
        report.contains("or stash the unstaged changes first."),
        "this is the case where staging is the remedy, so the report must say so: {report}"
    );
}

/// THE ARBITRARY FILE WRITE. A contributor stages a symlink whose target is an
/// absolute path outside the repository; the ordinary `git commit` gate then
/// runs a fixer over it. Neither the snapshot nor the write-back may follow that
/// link — the external file must be untouched, and the author must be told.
#[test]
fn write_back_never_writes_through_a_tracked_symlink_pointing_outside_the_repository() {
    let repo = init_repo();
    let cache = TempDir::new().expect("cache home");
    let outside = TempDir::new().expect("outside the repo");
    let root = repo.path();

    let victim = outside.path().join("authorized_keys");
    std::fs::write(&victim, "ORIGINAL\n").unwrap();
    std::os::unix::fs::symlink(&victim, root.join("evil.rs")).unwrap();
    git(root, &["add", "evil.rs"]);
    assert!(
        String::from_utf8_lossy(&git(root, &["ls-files", "-s", "evil.rs"]).stdout).starts_with("120000"),
        "precondition: the index holds a symlink entry"
    );

    let snapshot = StagedSnapshot::create_in(cache.path(), root).expect("snapshot");
    let outcome = run_isolated(root, snapshot.path(), fixer("evil.rs", "PWNED\\n"), &["evil.rs"]);

    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "ORIGINAL\n",
        "a file outside the repository must never be written by a hook fix"
    );
    assert!(
        std::fs::symlink_metadata(root.join("evil.rs"))
            .expect("entry present")
            .is_symlink(),
        "the worktree entry must be left exactly as the author has it"
    );
    assert!(!outcome.success(), "a fix that could not be delivered is not a pass");
    assert_eq!(
        outcome.stages[0].hooks[0].status,
        HookStatus::FixWithheld(vec![WithheldFix::new("evil.rs", WithheldReason::WorktreeIsSymlink)]),
        "a refusal to follow a symlink is not an unstaged-changes withhold"
    );
    // The report is the whole point of the refusal: told "you have unstaged
    // changes", the author stages and re-runs, learns nothing about the symlink,
    // and never looks at where it points.
    let report = report_of(&outcome);
    assert!(
        report.contains(
            "        SECURITY: evil.rs — the worktree entry is a symlink; \
             poly refused to write through it\n"
        ),
        "the refusal must name the file and say poly refused to write through it: {report}"
    );
    assert!(
        !report.contains("unstaged"),
        "a security refusal must never be reported as unstaged work: {report}"
    );
}

/// A relative link that stays inside the tree is legitimate content, so the
/// snapshot keeps it — but it is still never a write-back **destination**: poly
/// writes files, not the things files point at. The rewrite lands nowhere and
/// nothing in the worktree is disturbed.
#[test]
fn a_tracked_symlink_inside_the_repository_is_never_a_write_back_destination() {
    let repo = init_repo();
    let cache = TempDir::new().expect("cache home");
    let root = repo.path();
    std::fs::write(root.join("real.txt"), "real\n").unwrap();
    std::os::unix::fs::symlink("real.txt", root.join("link.txt")).unwrap();
    git(root, &["add", "."]);

    let snapshot = StagedSnapshot::create_in(cache.path(), root).expect("snapshot");
    run_isolated(root, snapshot.path(), fixer("link.txt", "fixed\\n"), &["link.txt"]);

    assert!(
        std::fs::symlink_metadata(root.join("link.txt"))
            .expect("entry present")
            .is_symlink(),
        "the link must not be replaced by a regular file"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("real.txt")).unwrap(),
        "real\n",
        "and nothing may be written through it either"
    );
}

/// The working case, which must keep working: the worktree copy is
/// byte-identical to the index, so the fix carries across, is staged, and the
/// file's mode survives the write (a fix must not disarm an executable script).
#[test]
fn write_back_lands_and_preserves_permissions_when_the_worktree_matches_the_index() {
    let repo = init_repo();
    let cache = TempDir::new().expect("cache home");
    let root = repo.path();
    let script = root.join("hook.sh");
    std::fs::write(&script, "#!/bin/sh\nunformatted\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(root, &["add", "hook.sh"]);

    let snapshot = StagedSnapshot::create_in(cache.path(), root).expect("snapshot");
    let outcome = run_isolated(
        root,
        snapshot.path(),
        fixer("hook.sh", "#!/bin/sh\\nformatted\\n"),
        &["hook.sh"],
    );

    assert!(outcome.success(), "report: {}", report_of(&outcome));
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        "#!/bin/sh\nformatted\n",
        "the fix must reach the worktree"
    );
    assert_eq!(staged_blob(root, "hook.sh"), "#!/bin/sh\nformatted\n");
    assert_eq!(
        std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755,
        "the executable bit must survive the write-back"
    );
    let leftovers: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".poly.tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the atomic write must leave no temp file: {leftovers:?}"
    );
}
