//! End-to-end tests for the native rayon hook runner (B1).
//!
//! Every test runs real subprocesses (`sh -c …`) inside a temporary git repo,
//! so the runner's stage order, priority grouping, `stage_fixed` re-staging,
//! and determinism are exercised against actual git/shell behaviour.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use poly_hooks::model::{HookCommand, HookStatus, StageStatus};
use poly_hooks::{Hook, HookRunReporter, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

// ── Helpers ─────────────────────────────────────────────────────────────────

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

/// A hook running a shell command, with no file matching (always runs, no
/// filenames appended) — the building block for ordering/abort tests.
fn cmd_hook(id: &str, command: &str) -> Hook {
    let mut hook = Hook::run(id, command);
    hook.always_run = true;
    hook.pass_filenames = false;
    hook
}

fn pre_commit(hooks: Vec<Hook>) -> StageSpec {
    StageSpec {
        stage: Stage::PreCommit,
        hooks,
        ..StageSpec::default()
    }
}

fn request(root: &Path, stage: StageSpec) -> HookRunRequest {
    HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![stage],
        ..HookRunRequest::default()
    }
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_default()
}

/// `true` when `name` has unstaged worktree modifications.
fn is_dirty(root: &Path, name: &str) -> bool {
    !Command::new("git")
        .args(["diff-files", "--quiet", "--", name])
        .current_dir(root)
        .status()
        .expect("git diff-files")
        .success()
}

// ── Priority-group ordering ──────────────────────────────────────────────────

#[test]
fn lower_priority_group_runs_before_higher() {
    let repo = init_repo();
    let root = repo.path();

    // hook "x" is declared first (position 0) but has higher priority value (0),
    // so it runs *after* "y" (priority -1). Each appends its id to out.txt.
    let mut x = cmd_hook("x", "printf x >> out.txt");
    x.priority = 0;
    let mut y = cmd_hook("y", "printf y >> out.txt");
    y.priority = -1;

    let outcome = run(request(root, pre_commit(vec![x, y]))).expect("run");

    assert!(outcome.success());
    // Sequential group boundaries → "y" (group -1) writes before "x" (group 0).
    assert_eq!(read(root, "out.txt"), "yx");
    // Outcomes are sorted by position: x (0) then y (1).
    let hooks = &outcome.stages[0].hooks;
    assert_eq!(hooks[0].id, "x");
    assert_eq!(hooks[1].id, "y");
    assert_eq!(hooks[0].position, 0);
    assert_eq!(hooks[1].position, 1);
}

// ── Parallel vs serial groups ─────────────────────────────────────────────────

#[test]
fn parallel_group_runs_every_hook() {
    let repo = init_repo();
    let root = repo.path();

    let hooks = (0..4)
        .map(|i| cmd_hook(&format!("h{i}"), &format!("printf x > h{i}.out")))
        .collect();
    let outcome = run(request(root, pre_commit(hooks))).expect("run");

    assert!(outcome.success());
    for i in 0..4 {
        assert_eq!(
            read(root, &format!("h{i}.out")),
            "x",
            "hook h{i} did not run"
        );
    }
    assert_eq!(outcome.stages[0].hooks.len(), 4);
}

#[test]
fn serial_group_runs_every_hook_when_require_serial() {
    let repo = init_repo();
    let root = repo.path();

    let hooks = (0..3)
        .map(|i| {
            let mut hook = cmd_hook(&format!("s{i}"), &format!("printf x > s{i}.out"));
            hook.require_serial = true; // forces the whole priority group serial
            hook
        })
        .collect();
    let outcome = run(request(root, pre_commit(hooks))).expect("run");

    assert!(outcome.success());
    for i in 0..3 {
        assert_eq!(read(root, &format!("s{i}.out")), "x");
    }
}

#[test]
fn single_thread_concurrency_forces_serial_and_passes() {
    let repo = init_repo();
    let root = repo.path();

    let hooks = (0..3)
        .map(|i| cmd_hook(&format!("j{i}"), &format!("printf x > j{i}.out")))
        .collect();
    let mut req = request(root, pre_commit(hooks));
    req.concurrency = Some(1); // -j1 / PREK_MAX_CONCURRENCY=1

    let outcome = run(req).expect("run");
    assert!(outcome.success());
    for i in 0..3 {
        assert_eq!(read(root, &format!("j{i}.out")), "x");
    }
}

// ── fail-fast ─────────────────────────────────────────────────────────────────

#[test]
fn fail_fast_aborts_later_priority_groups() {
    let repo = init_repo();
    let root = repo.path();

    let mut failing = cmd_hook("fail", "exit 1");
    failing.priority = -1;
    failing.fail_fast = true;
    let mut later = cmd_hook("later", "printf x > later.out");
    later.priority = 0;

    let outcome = run(request(root, pre_commit(vec![failing, later]))).expect("run");

    assert!(!outcome.success());
    // The higher-priority group never ran: only the failing hook is recorded.
    let hooks = &outcome.stages[0].hooks;
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].id, "fail");
    assert!(matches!(hooks[0].status, HookStatus::Failed { .. }));
    assert_eq!(read(root, "later.out"), "", "later hook must not have run");
}

#[test]
fn failure_without_fail_fast_still_runs_later_groups() {
    let repo = init_repo();
    let root = repo.path();

    let mut failing = cmd_hook("fail", "exit 1");
    failing.priority = -1;
    failing.fail_fast = false;
    let mut later = cmd_hook("later", "printf x > later.out");
    later.priority = 0;

    let outcome = run(request(root, pre_commit(vec![failing, later]))).expect("run");

    assert!(!outcome.success());
    assert_eq!(outcome.stages[0].hooks.len(), 2);
    assert_eq!(read(root, "later.out"), "x", "later hook should still run");
}

// ── stage_fixed re-staging ────────────────────────────────────────────────────

fn commit_and_stage_file(root: &Path, name: &str) {
    std::fs::write(root.join(name), "initial\n").unwrap();
    git(root, &["add", name]);
    git(root, &["commit", "-qm", "init"]);
    // Re-stage a fresh edit so the index matches the worktree (clean) before the
    // hook runs.
    std::fs::write(root.join(name), "staged\n").unwrap();
    git(root, &["add", name]);
}

#[test]
fn stage_fixed_restages_modified_files_and_continues() {
    let repo = init_repo();
    let root = repo.path();
    commit_and_stage_file(root, "f.txt");

    // A "formatter" that rewrites the matched file and exits 0.
    let mut hook = Hook::run("fmt", "echo formatted > f.txt");
    hook.pass_filenames = false;
    hook.stage_fixed = true;
    let stage = StageSpec {
        stage: Stage::PreCommit,
        hooks: vec![hook],
        ..StageSpec::default()
    };
    let mut req = request(root, stage);
    req.files = vec![PathBuf::from("f.txt")];

    let outcome = run(req).expect("run");

    assert!(outcome.success());
    assert!(outcome.stages[0].hooks[0].files_modified);
    // stage_fixed git-add'd the rewrite, so there is no unstaged diff left.
    assert!(!is_dirty(root, "f.txt"), "f.txt should have been re-staged");
}

#[test]
fn modification_left_unstaged_when_not_stage_fixed() {
    let repo = init_repo();
    let root = repo.path();
    commit_and_stage_file(root, "g.txt");

    let mut hook = Hook::run("fmt", "echo formatted > g.txt");
    hook.pass_filenames = false;
    hook.stage_fixed = false;
    let stage = StageSpec {
        stage: Stage::PreCommit,
        hooks: vec![hook],
        ..StageSpec::default()
    };
    let mut req = request(root, stage);
    req.files = vec![PathBuf::from("g.txt")];

    let outcome = run(req).expect("run");

    assert!(outcome.success());
    assert!(!outcome.stages[0].hooks[0].files_modified);
    // The rewrite stays in the worktree, unstaged.
    assert!(
        is_dirty(root, "g.txt"),
        "g.txt modification should be unstaged"
    );
}

// ── precondition skip ─────────────────────────────────────────────────────────

#[test]
fn failing_precondition_skips_stage() {
    let repo = init_repo();
    let root = repo.path();

    let stage = StageSpec {
        stage: Stage::PreCommit,
        precondition: Some("exit 1".to_string()),
        hooks: vec![cmd_hook("h", "printf x > h.out")],
        ..StageSpec::default()
    };
    let outcome = run(request(root, stage)).expect("run");

    assert!(matches!(outcome.stages[0].status, StageStatus::Skipped(_)));
    assert!(outcome.stages[0].hooks.is_empty());
    assert_eq!(
        read(root, "h.out"),
        "",
        "hook must not run when precondition fails"
    );
    // A skipped stage is not a failure.
    assert!(outcome.success());
}

#[test]
fn passing_precondition_runs_stage() {
    let repo = init_repo();
    let root = repo.path();

    let stage = StageSpec {
        stage: Stage::PreCommit,
        precondition: Some("true".to_string()),
        hooks: vec![cmd_hook("h", "printf x > h.out")],
        ..StageSpec::default()
    };
    let outcome = run(request(root, stage)).expect("run");

    assert!(matches!(outcome.stages[0].status, StageStatus::Ran));
    assert_eq!(read(root, "h.out"), "x");
}

// ── before / after steps ──────────────────────────────────────────────────────

#[test]
fn failing_before_aborts_stage() {
    let repo = init_repo();
    let root = repo.path();

    let stage = StageSpec {
        stage: Stage::PreCommit,
        before: vec!["exit 3".to_string()],
        hooks: vec![cmd_hook("h", "printf x > h.out")],
        ..StageSpec::default()
    };
    let outcome = run(request(root, stage)).expect("run");

    assert!(matches!(outcome.stages[0].status, StageStatus::Aborted(_)));
    assert!(!outcome.success());
    assert!(outcome.stages[0].hooks.is_empty());
    assert_eq!(
        read(root, "h.out"),
        "",
        "hooks must not run after a failed before step"
    );
}

#[test]
fn after_runs_only_when_hooks_succeed() {
    let repo = init_repo();
    let root = repo.path();

    let stage = StageSpec {
        stage: Stage::PreCommit,
        after: vec!["printf done > after.out".to_string()],
        hooks: vec![cmd_hook("h", "true")],
        ..StageSpec::default()
    };
    let outcome = run(request(root, stage)).expect("run");

    assert!(outcome.success());
    assert_eq!(outcome.stages[0].after.len(), 1);
    assert_eq!(read(root, "after.out"), "done");
}

#[test]
fn after_skipped_when_a_hook_fails() {
    let repo = init_repo();
    let root = repo.path();

    let stage = StageSpec {
        stage: Stage::PreCommit,
        after: vec!["printf done > after.out".to_string()],
        hooks: vec![cmd_hook("h", "exit 1")],
        ..StageSpec::default()
    };
    let outcome = run(request(root, stage)).expect("run");

    assert!(!outcome.success());
    assert!(outcome.stages[0].after.is_empty());
    assert_eq!(
        read(root, "after.out"),
        "",
        "after must not run when a hook failed"
    );
}

// ── deterministic, non-interleaved output ──────────────────────────────────────

#[test]
fn output_is_deterministic_and_non_interleaved() {
    let repo = init_repo();
    let root = repo.path();

    // Two failing hooks each emit a distinct two-line block. Even though they run
    // in parallel, capture-then-render must keep each block contiguous and in
    // position order, and produce identical output across runs.
    let make_hooks = || {
        vec![
            cmd_hook("alpha", "printf 'A1\\nA2\\n'; exit 1"),
            cmd_hook("beta", "printf 'B1\\nB2\\n'; exit 1"),
        ]
    };

    let outcome1 = run(request(root, pre_commit(make_hooks()))).expect("run");
    let outcome2 = run(request(root, pre_commit(make_hooks()))).expect("run");

    let reporter = HookRunReporter::new();
    let report1 = reporter.render(&outcome1);
    let report2 = reporter.render(&outcome2);

    assert_eq!(report1, report2, "render must be deterministic");

    // alpha's block precedes beta's block, and each block is contiguous.
    let alpha_idx = report1.find("alpha").expect("alpha present");
    let beta_idx = report1.find("beta").expect("beta present");
    assert!(alpha_idx < beta_idx, "alpha must render before beta");

    let a1 = report1.find("A1").unwrap();
    let a2 = report1.find("A2").unwrap();
    let b1 = report1.find("B1").unwrap();
    // alpha's two lines are adjacent (no beta output interleaved between them).
    assert!(
        a1 < a2 && a2 < b1,
        "alpha block must be contiguous and before beta"
    );
}

#[test]
fn hook_command_script_form_executes() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("s.sh"), "#!/bin/sh\nprintf ran > script.out\n").unwrap();

    let mut hook = Hook {
        id: "script".to_string(),
        command: HookCommand::Script {
            path: "s.sh".to_string(),
            runner: Some("sh".to_string()),
        },
        ..Hook::default()
    };
    hook.always_run = true;
    hook.pass_filenames = false;

    let outcome = run(request(root, pre_commit(vec![hook]))).expect("run");
    assert!(outcome.success());
    assert_eq!(read(root, "script.out"), "ran");
}
