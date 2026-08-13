//! Time budgets for the parts of a run that are not the hook body: stage-level
//! `precondition` / `before` / `after` steps, per-hook preconditions, and the
//! environment overrides that win over a configured budget.
//!
//! Every test here mutates the process environment, which is global, so they
//! are serialised through [`Env`] — `set_var` races any concurrent reader,
//! including the `Command::spawn` these tests perform. Budgets are sub-second
//! so a wedged step is killed while the suite still runs fast.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use poly_hooks::model::HookStatus;
use poly_hooks::timeout::{
    DEFAULT_HOOK_TIMEOUT, DEFAULT_PRECONDITION_TIMEOUT, DEFAULT_STEP_TIMEOUT, DEFAULT_WORKSPACE_HOOK_TIMEOUT,
    HOOK_TIMEOUT_ENV, HookTimeout, PRECONDITION_TIMEOUT_ENV, STEP_TIMEOUT_ENV, WORKSPACE_HOOK_TIMEOUT_ENV, budget_for,
    precondition_budget, step_budget,
};
use poly_hooks::{Hook, HookRunReporter, HookRunRequest, Stage, StageSpec, StageStatus, run};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A scoped set of environment variables, serialised across this test binary.
///
/// The keys are removed before the lock is released — including on a panic —
/// so a failing test cannot leak a budget into the next one.
struct Env {
    keys: Vec<&'static str>,
    _lock: MutexGuard<'static, ()>,
}

impl Env {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            _lock: ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner),
        }
    }

    fn set(&mut self, key: &'static str, value: &str) -> &mut Self {
        self.keys.push(key);
        // SAFETY: every environment mutation in this binary happens while
        // holding `ENV_LOCK`, and the variable is removed before the lock is
        // released, so no other thread reads the environment concurrently.
        unsafe { std::env::set_var(key, value) };
        self
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for key in &self.keys {
            // SAFETY: as in `set` — still holding `ENV_LOCK`.
            unsafe { std::env::remove_var(key) };
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation");
    assert!(output.status.success(), "git {args:?} failed");
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "test@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}

fn cmd_hook(id: &str, command: &str) -> Hook {
    let mut hook = Hook::run(id, command);
    hook.always_run = true;
    hook.pass_filenames = false;
    hook
}

fn request(root: &Path, stage: StageSpec) -> HookRunRequest {
    HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![stage],
        ..HookRunRequest::default()
    }
}

fn stage_spec(spec: StageSpec) -> StageSpec {
    StageSpec {
        stage: Stage::PreCommit,
        ..spec
    }
}

/// The shell line a "leaves no orphan" assertion needs: record the shell's own
/// pid and a backgrounded grandchild's, then wait forever.
const SPAWNER: &str = "printf '%s' \"$$\" > shell.pid; sleep 30 & printf '%s' \"$!\" > child.pid; wait";

/// Poll `kill -0 <pid>` until the process is gone, up to five seconds.
fn wait_until_gone(pid: &str) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let alive = Command::new("kill")
            .args(["-0", pid])
            .output()
            .expect("kill -0")
            .status
            .success();
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Precedence: the environment overrides a configured budget
// ---------------------------------------------------------------------------

/// The environment variable is the escape hatch of whoever is running the
/// hooks, so it wins over a budget the config author chose — in both
/// directions: without it, the configured budget stands.
#[test]
fn environment_override_beats_a_configured_hook_timeout() {
    let mut env = Env::new();

    let mut hook = Hook::run("slow", "sleep 30");
    hook.timeout = HookTimeout::Limit(Duration::from_mins(10));
    assert_eq!(
        budget_for(&hook).limit,
        Some(Duration::from_mins(10)),
        "with no override the configured budget stands"
    );

    env.set(HOOK_TIMEOUT_ENV, "1");
    assert_eq!(
        budget_for(&hook).limit,
        Some(Duration::from_secs(1)),
        "the environment override wins over the configured budget"
    );
}

/// The same for a whole-project hook, through its own variable.
#[test]
fn workspace_environment_override_beats_a_configured_hook_timeout() {
    let mut env = Env::new();

    let mut hook = Hook::run("clippy", "cargo clippy");
    hook.workspace = true;
    hook.timeout = HookTimeout::Limit(Duration::from_mins(1));
    assert_eq!(budget_for(&hook).limit, Some(Duration::from_mins(1)));

    env.set(WORKSPACE_HOOK_TIMEOUT_ENV, "2m");
    assert_eq!(budget_for(&hook).limit, Some(Duration::from_mins(2)));
}

/// Disabling must be total: a hook that set its own budget is unbounded too,
/// and drops back to the un-supervised execution path.
#[test]
fn environment_disable_beats_a_configured_hook_timeout() {
    let mut env = Env::new();
    env.set(HOOK_TIMEOUT_ENV, "0");

    let mut hook = Hook::run("slow", "sleep 30");
    hook.timeout = HookTimeout::Limit(Duration::from_mins(10));

    let budget = budget_for(&hook);
    assert_eq!(budget.limit, None, "the disable form removes the limit");
    assert!(
        !budget.is_supervised(),
        "a disabled budget restores the un-supervised path"
    );
}

// ---------------------------------------------------------------------------
// Stage-level `before` / `after`
// ---------------------------------------------------------------------------

/// A stage `before` that never returns is the same defect the hook timeouts
/// were added for: it must be killed, the run must fail, and the report must
/// say it was **killed** rather than that it failed on its own merits.
#[test]
fn stage_before_that_overruns_is_killed_and_fails_the_run() {
    let mut env = Env::new();
    env.set(STEP_TIMEOUT_ENV, "300ms");

    let repo = init_repo();
    let root = repo.path();
    let spec = stage_spec(StageSpec {
        before: vec!["sleep 30".to_string()],
        hooks: vec![cmd_hook("fmt", "printf x > ran.out")],
        ..StageSpec::default()
    });

    let outcome = run(request(root, spec)).expect("run");

    assert!(!outcome.success(), "a killed stage `before` must fail the run");
    let stage = &outcome.stages[0];
    let HookStatus::TimedOut(reason) = &stage.before[0].status else {
        panic!("expected the step to be killed, got {:?}", stage.before[0].status);
    };
    assert_eq!(reason.limit, Duration::from_millis(300));
    assert!(reason.elapsed >= Duration::from_millis(300));
    assert!(matches!(stage.status, StageStatus::Aborted(_)), "{:?}", stage.status);
    assert!(
        matches!(stage.hooks[0].status, HookStatus::Unknown(_)),
        "the hooks after a killed setup step validated nothing: {:?}",
        stage.hooks[0].status
    );
    assert!(!root.join("ran.out").exists(), "the stage's hooks must not have run");
    assert_eq!(outcome.verdict_count(), 0);

    let report = HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("timed out: poly killed it after"),
        "the report must say poly killed the step: {report}"
    );
    assert!(report.contains('⧖'), "a killed step gets the timeout marker: {report}");
    assert!(
        !report.contains("before step failed"),
        "a killed step must not be reported as a failure of its own: {report}"
    );
    assert!(
        report.contains("before step timed out: sleep 30"),
        "the abort names the killed step: {report}"
    );
}

/// The kill must reap the step's process tree, not merely stop waiting on it.
#[test]
fn killed_stage_before_leaves_no_surviving_process() {
    let mut env = Env::new();
    env.set(STEP_TIMEOUT_ENV, "300ms");

    let repo = init_repo();
    let root = repo.path();
    let spec = stage_spec(StageSpec {
        before: vec![SPAWNER.to_string()],
        hooks: vec![cmd_hook("fmt", "true")],
        ..StageSpec::default()
    });

    let outcome = run(request(root, spec)).expect("run");
    assert!(!outcome.success());

    let shell_pid = read(root, "shell.pid");
    let child_pid = read(root, "child.pid");
    assert!(!shell_pid.is_empty(), "the step must have recorded its shell pid");
    assert!(!child_pid.is_empty(), "the step must have recorded its child pid");
    assert!(wait_until_gone(&shell_pid), "the step's shell ({shell_pid}) survived");
    assert!(
        wait_until_gone(&child_pid),
        "the step's child process ({child_pid}) survived the kill"
    );
}

/// An `after` step is bounded on the same terms.
#[test]
fn stage_after_that_overruns_is_killed_and_fails_the_run() {
    let mut env = Env::new();
    env.set(STEP_TIMEOUT_ENV, "300ms");

    let repo = init_repo();
    let spec = stage_spec(StageSpec {
        after: vec!["sleep 30".to_string()],
        hooks: vec![cmd_hook("fmt", "true")],
        ..StageSpec::default()
    });

    let outcome = run(request(repo.path(), spec)).expect("run");

    assert!(!outcome.success());
    assert!(matches!(outcome.stages[0].after[0].status, HookStatus::TimedOut(_)));
    let report = HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("after step timed out: sleep 30"),
        "the abort names the killed step: {report}"
    );
}

// ---------------------------------------------------------------------------
// Preconditions
// ---------------------------------------------------------------------------

/// A wedged applicability probe must never be read as "does not apply": that
/// would skip every hook in the stage and report success having validated
/// nothing. It aborts the stage instead.
#[test]
fn stage_precondition_that_overruns_aborts_the_stage_rather_than_skipping_it() {
    let mut env = Env::new();
    env.set(PRECONDITION_TIMEOUT_ENV, "300ms");

    let repo = init_repo();
    let root = repo.path();
    let spec = stage_spec(StageSpec {
        precondition: Some("sleep 30".to_string()),
        hooks: vec![cmd_hook("fmt", "printf x > ran.out")],
        ..StageSpec::default()
    });

    let outcome = run(request(root, spec)).expect("run");

    assert!(!outcome.success(), "a killed precondition must fail the run");
    let stage = &outcome.stages[0];
    assert!(matches!(stage.status, StageStatus::Aborted(_)), "{:?}", stage.status);
    let HookStatus::TimedOut(reason) = &stage.hooks[0].status else {
        panic!("expected a timeout status, got {:?}", stage.hooks[0].status);
    };
    assert_eq!(reason.limit, Duration::from_millis(300));
    assert_eq!(
        outcome.precondition_skipped_count(),
        0,
        "a killed probe is not a benign skip"
    );
    assert!(!root.join("ran.out").exists());

    let report = HookRunReporter::new().render(&outcome);
    assert!(
        report.contains("precondition timed out: poly killed it after"),
        "the report must name the killed probe: {report}"
    );
}

/// A hook's own precondition is scoped to that hook: killing it leaves the
/// siblings reporting real verdicts.
#[test]
fn hook_precondition_that_overruns_marks_only_that_hook() {
    let mut env = Env::new();
    env.set(PRECONDITION_TIMEOUT_ENV, "300ms");

    let repo = init_repo();
    let mut wedged = cmd_hook("wedged-probe", "printf x > wedged.out");
    wedged.precondition = Some("sleep 30".to_string());
    let healthy = cmd_hook("healthy", "printf x > healthy.out");

    let outcome = run(request(
        repo.path(),
        stage_spec(StageSpec {
            hooks: vec![wedged, healthy],
            ..StageSpec::default()
        }),
    ))
    .expect("run");

    assert!(!outcome.success());
    assert!(
        matches!(outcome.stages[0].hooks[0].status, HookStatus::TimedOut(_)),
        "{:?}",
        outcome.stages[0].hooks[0].status
    );
    assert_eq!(outcome.stages[0].hooks[1].status, HookStatus::Passed);
    assert!(!repo.path().join("wedged.out").exists());
    assert_eq!(read(repo.path(), "healthy.out"), "x");
}

// ---------------------------------------------------------------------------
// Ordinary runs are unaffected
// ---------------------------------------------------------------------------

/// Steps and probes that finish inside their budget behave exactly as before:
/// no kills, no extra lines, no markers legend.
#[test]
fn steps_and_probes_within_budget_are_unaffected() {
    let mut env = Env::new();
    env.set(STEP_TIMEOUT_ENV, "30s");
    env.set(PRECONDITION_TIMEOUT_ENV, "30s");

    let repo = init_repo();
    let root = repo.path();
    let mut hook = cmd_hook("fmt", "printf ran > hook.out");
    hook.precondition = Some("true".to_string());
    let spec = stage_spec(StageSpec {
        precondition: Some("true".to_string()),
        before: vec!["printf b > before.out".to_string()],
        after: vec!["printf a > after.out".to_string()],
        hooks: vec![hook],
        ..StageSpec::default()
    });

    let outcome = run(request(root, spec)).expect("run");

    assert!(outcome.success(), "nothing here overruns anything");
    assert_eq!(read(root, "before.out"), "b");
    assert_eq!(read(root, "hook.out"), "ran");
    assert_eq!(read(root, "after.out"), "a");

    let report = HookRunReporter::new().render(&outcome);
    assert!(report.contains("before: printf b > before.out"), "{report}");
    assert!(!report.contains("timed out"), "no timeout noise: {report}");
    assert!(!report.contains("still running"), "no liveness noise: {report}");
    assert!(!report.contains("markers:"), "no legend needed: {report}");
}

/// The default budgets: setup steps get the per-file hook budget, probes a far
/// shorter one — a probe that needs minutes is not a probe.
#[test]
fn step_and_precondition_defaults_are_ordered_against_the_hook_defaults() {
    let _env = Env::new();

    assert_eq!(step_budget().limit, Some(DEFAULT_STEP_TIMEOUT));
    assert_eq!(precondition_budget().limit, Some(DEFAULT_PRECONDITION_TIMEOUT));
    assert_eq!(DEFAULT_STEP_TIMEOUT, DEFAULT_HOOK_TIMEOUT);
    assert!(DEFAULT_PRECONDITION_TIMEOUT < DEFAULT_STEP_TIMEOUT);
    assert!(DEFAULT_STEP_TIMEOUT < DEFAULT_WORKSPACE_HOOK_TIMEOUT);
}

/// Both step budgets are disableable, and disabling restores the un-supervised
/// path exactly as it does for a hook.
#[test]
fn step_and_precondition_budgets_can_be_disabled() {
    let mut env = Env::new();
    env.set(STEP_TIMEOUT_ENV, "off");
    env.set(PRECONDITION_TIMEOUT_ENV, "none");

    assert!(!step_budget().is_supervised());
    assert!(!precondition_budget().is_supervised());
}

/// A budget the environment cannot parse is ignored with a warning rather than
/// silently disabling the limit — the fail-open reading of a typo is the one
/// outcome that reintroduces the hang.
#[test]
fn an_unparseable_environment_budget_falls_back_to_the_default() {
    let mut env = Env::new();
    env.set(HOOK_TIMEOUT_ENV, "soon");

    let hook = Hook::run("fmt", "cargo fmt");
    assert_eq!(budget_for(&hook).limit, Some(DEFAULT_HOOK_TIMEOUT));
}
