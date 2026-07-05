//! End-to-end tests for the native rayon hook runner (B1).
//!
//! Every test runs real subprocesses (`sh -c …`) inside a temporary git repo,
//! so the runner's stage order, priority grouping, `stage_fixed` re-staging,
//! and determinism are exercised against actual git/shell behaviour.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use poly_cache::ResultCache;
use poly_hooks::filter::FilePattern;
use poly_hooks::model::{HookCache, HookCommand, HookStatus, StageStatus};
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
        assert_eq!(read(root, &format!("h{i}.out")), "x", "hook h{i} did not run");
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
    assert!(is_dirty(root, "g.txt"), "g.txt modification should be unstaged");
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
    assert_eq!(read(root, "h.out"), "", "hook must not run when precondition fails");
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
    assert_eq!(read(root, "h.out"), "", "hooks must not run after a failed before step");
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
    assert_eq!(read(root, "after.out"), "", "after must not run when a hook failed");
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
    assert!(a1 < a2 && a2 < b1, "alpha block must be contiguous and before beta");
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

// ── tier-1 result caching ───────────────────────────────────────────────────

/// An enabled result cache rooted in its own temp dir (isolated from the repo).
fn cache_at(dir: &TempDir) -> ResultCache {
    ResultCache::open(dir.path().join("cache"), true).expect("open cache")
}

/// Commit a tracked file so `git ls-files` lists it and the worktree is clean.
fn commit_tracked(root: &Path, name: &str, contents: &str) {
    std::fs::write(root.join(name), contents).unwrap();
    git(root, &["add", name]);
    git(root, &["commit", "-qm", "init"]);
}

#[test]
fn matched_files_hook_is_cached_on_second_identical_run() {
    let repo = init_repo();
    let root = repo.path();
    commit_tracked(root, "input.txt", "data\n");
    let cache_dir = TempDir::new().unwrap();

    // Each real execution appends to runs.log; a cache hit must skip the body.
    let make = || {
        let mut hook = Hook::run("counter", "printf x >> runs.log");
        hook.pass_filenames = false;
        hook.cache = HookCache::MatchedFiles;
        hook
    };
    let build = || {
        let mut req = request(root, pre_commit(vec![make()]));
        req.files = vec![PathBuf::from("input.txt")];
        req.cache = Some(cache_at(&cache_dir));
        req
    };

    let first = run(build()).expect("run");
    assert!(first.success());
    assert!(!first.stages[0].hooks[0].cached, "first run is a miss");
    assert_eq!(read(root, "runs.log"), "x");

    let second = run(build()).expect("run");
    assert!(second.success());
    assert!(second.stages[0].hooks[0].cached, "second run must hit");
    assert_eq!(read(root, "runs.log"), "x", "cache hit must not re-execute");
}

#[test]
fn editing_a_declared_input_invalidates_the_cache() {
    let repo = init_repo();
    let root = repo.path();
    commit_tracked(root, "input.txt", "v1\n");
    let cache_dir = TempDir::new().unwrap();

    let make = || {
        let mut hook = Hook::run("c", "printf x >> runs.log");
        hook.pass_filenames = false;
        hook.always_run = true;
        hook.cache = HookCache::DeclaredInputs(FilePattern::glob(vec!["**/*.txt".into()]).unwrap());
        hook
    };
    let build = || {
        let mut req = request(root, pre_commit(vec![make()]));
        req.cache = Some(cache_at(&cache_dir));
        req
    };

    run(build()).expect("run"); // miss → executes
    assert_eq!(read(root, "runs.log"), "x");
    let hit = run(build()).expect("run"); // unchanged → hit
    assert!(hit.stages[0].hooks[0].cached);
    assert_eq!(read(root, "runs.log"), "x");

    // Change the declared input; the next run must re-execute.
    std::fs::write(root.join("input.txt"), "v2\n").unwrap();
    let miss = run(build()).expect("run");
    assert!(!miss.stages[0].hooks[0].cached, "edit must invalidate");
    assert_eq!(read(root, "runs.log"), "xx");
}

#[test]
fn a_hook_that_modifies_its_inputs_is_never_cached() {
    let repo = init_repo();
    let root = repo.path();
    commit_tracked(root, "f.txt", "orig\n");
    let cache_dir = TempDir::new().unwrap();

    // The hook rewrites its matched file (a worktree diff) and logs each run.
    let make = || {
        let mut hook = Hook::run("fixer", "printf changed > f.txt; printf x >> runs.log");
        hook.pass_filenames = false;
        hook.cache = HookCache::MatchedFiles;
        hook
    };
    let build = || {
        let mut req = request(root, pre_commit(vec![make()]));
        req.files = vec![PathBuf::from("f.txt")];
        req.cache = Some(cache_at(&cache_dir));
        req
    };

    run(build()).expect("run");
    let second = run(build()).expect("run");
    assert!(!second.stages[0].hooks[0].cached, "tree-mutating hook must not cache");
    assert_eq!(read(root, "runs.log"), "xx", "must execute on both runs");
}

#[test]
fn declared_inputs_hook_that_mutates_an_input_is_never_cached() {
    let repo = init_repo();
    let root = repo.path();
    commit_tracked(root, "dep.txt", "orig\n");
    let cache_dir = TempDir::new().unwrap();

    // Declares `**/*.txt` as inputs but also REWRITES dep.txt — a mutation to a
    // declared input outside the (empty) matched set. It must never be cached,
    // even though its pre-execution digest is stable once dep.txt is reverted.
    let make = || {
        let mut hook = Hook::run("mutator", "printf x >> runs.log; printf changed > dep.txt");
        hook.pass_filenames = false;
        hook.always_run = true;
        hook.cache = HookCache::DeclaredInputs(FilePattern::glob(vec!["**/*.txt".into()]).unwrap());
        hook
    };
    let build = || {
        let mut req = request(root, pre_commit(vec![make()]));
        req.cache = Some(cache_at(&cache_dir));
        req
    };

    run(build()).expect("run"); // executes, mutates dep.txt → must NOT store
    assert_eq!(read(root, "runs.log"), "x");

    // Revert dep.txt so the pre-execution digest matches the first run. Had the
    // first run wrongly stored, this would be a hit and skip execution.
    std::fs::write(root.join("dep.txt"), "orig\n").unwrap();
    let second = run(build()).expect("run");
    assert!(
        !second.stages[0].hooks[0].cached,
        "a hook that mutated a declared input must never be cached"
    );
    assert_eq!(read(root, "runs.log"), "xx", "must re-execute, not hit");
}

#[test]
fn cache_none_bypasses_caching_entirely() {
    let repo = init_repo();
    let root = repo.path();
    commit_tracked(root, "input.txt", "data\n");
    let cache_dir = TempDir::new().unwrap();

    let make = || {
        let mut hook = Hook::run("counter", "printf x >> runs.log");
        hook.pass_filenames = false;
        hook.cache = HookCache::MatchedFiles;
        hook
    };

    // First run WITH cache stores an entry.
    let mut req1 = request(root, pre_commit(vec![make()]));
    req1.files = vec![PathBuf::from("input.txt")];
    req1.cache = Some(cache_at(&cache_dir));
    run(req1).expect("run");
    assert_eq!(read(root, "runs.log"), "x");

    // Second run with cache = None must bypass the stored entry and re-execute.
    let mut req2 = request(root, pre_commit(vec![make()]));
    req2.files = vec![PathBuf::from("input.txt")];
    req2.cache = None;
    let second = run(req2).expect("run");
    assert!(!second.stages[0].hooks[0].cached);
    assert_eq!(read(root, "runs.log"), "xx", "cache=None must re-execute");
}

/// A `workspace` hook runs from `work_root` (the staged snapshot) while per-file
/// hooks stay at `root`, and cargo is redirected at the real `target/`.
#[test]
fn workspace_hook_runs_in_work_root_with_cargo_target_dir() {
    let repo = init_repo();
    let root = repo.path();
    let snap = TempDir::new().expect("snapshot dir");
    let snap_path = snap.path();

    let mut workspace_hook = cmd_hook(
        "ws",
        "echo ws > marker.txt && printf '%s' \"$CARGO_TARGET_DIR\" > ct.txt",
    );
    workspace_hook.workspace = true;
    let per_file_hook = cmd_hook("per_file", "echo pf > marker.txt");

    let req = HookRunRequest {
        root: root.to_path_buf(),
        work_root: Some(snap_path.to_path_buf()),
        stages: vec![pre_commit(vec![workspace_hook, per_file_hook])],
        ..HookRunRequest::default()
    };
    let outcome = run(req).expect("run");
    assert!(outcome.success());

    // The workspace hook's cwd was the snapshot: its marker landed there, not in root.
    assert_eq!(read(snap_path, "marker.txt").trim(), "ws");
    // The per-file hook ran from root and overwrote nothing in the snapshot.
    assert_eq!(read(root, "marker.txt").trim(), "pf");
    // cargo is pointed at the real repo's target dir, not the snapshot's.
    assert_eq!(read(snap_path, "ct.txt"), root.join("target").to_string_lossy());
}

/// Without a `work_root`, a `workspace` hook runs from `root` like any other —
/// isolation is opt-in per run, not implied by the flag.
#[test]
fn workspace_hook_without_work_root_runs_in_root() {
    let repo = init_repo();
    let root = repo.path();

    let mut workspace_hook = cmd_hook(
        "ws",
        "echo ws > marker.txt && printf '%s' \"${CARGO_TARGET_DIR:-unset}\" > ct.txt",
    );
    workspace_hook.workspace = true;

    let outcome = run(request(root, pre_commit(vec![workspace_hook]))).expect("run");
    assert!(outcome.success());
    assert_eq!(read(root, "marker.txt").trim(), "ws");
    // No isolation → CARGO_TARGET_DIR is left untouched.
    assert_eq!(read(root, "ct.txt"), "unset");
}

/// A workspace hook's result-cache key is derived from STAGED content (the
/// snapshot at `work_root`), not the worktree: editing the worktree copy of a
/// tracked input leaves a cache hit intact, while editing the snapshot copy
/// busts it. This is what makes caching safe under isolation.
#[test]
fn workspace_hook_cache_key_follows_staged_snapshot_not_worktree() {
    let repo = init_repo();
    let root = repo.path();
    std::fs::write(root.join("in.rs"), "STAGED").unwrap();
    git(root, &["add", "in.rs"]);

    // The snapshot holds the staged content; `runs.log` (not matched by the
    // `*.rs` cache glob) records each real execution so hits are observable.
    let snap = TempDir::new().expect("snapshot");
    std::fs::write(snap.path().join("in.rs"), "STAGED").unwrap();
    let cache_dir = TempDir::new().expect("cache");

    let hook = || {
        let mut hook = cmd_hook("ws", "echo ran >> runs.log");
        hook.workspace = true;
        hook.cache = HookCache::DeclaredInputs(FilePattern::glob(vec!["*.rs".to_string()]).unwrap());
        hook
    };
    let run_once = || {
        let mut req = HookRunRequest {
            root: root.to_path_buf(),
            work_root: Some(snap.path().to_path_buf()),
            stages: vec![pre_commit(vec![hook()])],
            ..HookRunRequest::default()
        };
        req.cache = Some(cache_at(&cache_dir));
        run(req).expect("run");
    };

    run_once(); // miss → executes
    run_once(); // hit → skipped
    assert_eq!(
        read(snap.path(), "runs.log").lines().count(),
        1,
        "second run must hit the cache"
    );

    // Editing the WORKTREE copy must not bust the key (staged content unchanged).
    std::fs::write(root.join("in.rs"), "WORKTREE-DIRTY").unwrap();
    run_once();
    assert_eq!(
        read(snap.path(), "runs.log").lines().count(),
        1,
        "worktree edit must not invalidate"
    );

    // Editing the SNAPSHOT (staged) copy busts the key → re-executes.
    std::fs::write(snap.path().join("in.rs"), "STAGED-CHANGED").unwrap();
    run_once();
    assert_eq!(
        read(snap.path(), "runs.log").lines().count(),
        2,
        "staged change must invalidate"
    );
}
