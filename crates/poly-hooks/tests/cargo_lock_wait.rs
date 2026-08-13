//! A cargo hook must not be charged for a lock held outside the run.
//!
//! The defect: cargo serialises on `$CARGO_HOME/.package-cache`, so a cargo hook
//! can sit blocked — printing nothing, doing nothing — behind rust-analyzer or a
//! developer's own `cargo build`, and be killed for waiting rather than for
//! hanging. Poly's own cargo hooks never overlap (ADR 0024's exclusion set), so
//! the holder is always one poly did not start and cannot schedule.
//!
//! These tests drive the real runner with a real `flock` held on a real file,
//! and pin the property that matters: the hook's clock starts when the *hook*
//! starts, not when the run reaches it. The hook itself stands in for cargo —
//! it blocks on a sentinel removed at the moment the lock is released — because
//! only a child that is actually blocked can distinguish a budget that started
//! too early from one that did not.
//!
//! `sh` command lines and `flock` make this Unix-only, as with the rest of the
//! runner's process-level tests.
#![cfg(unix)]

use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use poly_hooks::consts::env_vars::EnvVars;
use poly_hooks::model::{CARGO_SERIAL_GROUP, HookStatus};
use poly_hooks::timeout::HOOK_TIMEOUT_ENV;
use poly_hooks::{Hook, HookRunRequest, Stage, StageSpec, run};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Timings
//
// The three are chosen against each other, so that the fixed and broken
// behaviours land on opposite sides of the budget with half a second of margin
// each way:
//
//   HOLD + WORK  >  LIMIT   — a clock started at the run's cargo phase is
//                             overrun, and the hook is killed.
//   WORK         <  LIMIT   — a clock started when the lock cleared is not.
//   HOLD         <= LIMIT/2 — the hold fits inside the derived wait bound, so
//                             the wait ends at the release rather than expiring.
// ---------------------------------------------------------------------------

/// How long the external holder keeps cargo's package-cache lock.
const HOLD: Duration = Duration::from_secs(1);
/// How long the hook works for once it is unblocked.
const WORK: Duration = Duration::from_millis(2_500);
/// The hook's whole time budget.
const LIMIT: Duration = Duration::from_secs(3);

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A scoped set of environment variables, serialised across this test binary.
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

/// Hold a real exclusive `flock` on `$CARGO_HOME/.package-cache` for [`HOLD`],
/// then remove `sentinel` and release — the two events a real cargo would
/// couple, since the lock clearing is what lets the blocked cargo proceed.
///
/// `flock` is per-open-file-description, so a hold taken here contends with the
/// runner's probe exactly as another process's would.
fn hold_package_cache(cargo_home: &Path, sentinel: &Path) -> std::thread::JoinHandle<()> {
    let lock = cargo_home.join(".package-cache");
    let sentinel = sentinel.to_path_buf();
    let (ready, holding) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut file = std::fs::File::create(&lock).expect("create the package-cache lock");
        file.write_all(b"held").expect("write the package-cache lock");
        // SAFETY: the descriptor is owned by `file`, which outlives the call.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(taken, 0, "the holder must actually acquire the lock");
        ready.send(()).expect("signal that the lock is held");
        std::thread::sleep(HOLD);
        std::fs::remove_file(&sentinel).expect("release the blocked hook");
        // SAFETY: as above.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    });
    holding.recv().expect("wait until the lock is held");
    handle
}

/// A hook that stands in for cargo: it blocks while `sentinel` exists — as a
/// cargo subcommand blocks on the package-cache lock — and only then works.
fn blocked_cargo_hook(sentinel: &Path) -> Hook {
    let sentinel = sentinel.display();
    let work = WORK.as_secs_f64();
    let mut hook = Hook::run(
        "cargo:deny",
        format!("while [ -e '{sentinel}' ]; do sleep 0.05; done; sleep {work}"),
    );
    hook.always_run = true;
    hook.pass_filenames = false;
    // The classifier, set exactly as lowering sets it for a cargo command line.
    hook.serial_group = Some(CARGO_SERIAL_GROUP.to_string());
    hook
}

fn run_one(root: &Path, hook: Hook) -> poly_hooks::HookOutcome {
    let request = HookRunRequest {
        root: root.to_path_buf(),
        stages: vec![StageSpec {
            stage: Stage::PreCommit,
            hooks: vec![hook],
            ..StageSpec::default()
        }],
        ..HookRunRequest::default()
    };
    let outcome = run(request).expect("the run itself must not fail");
    let mut hooks = outcome.stages.into_iter().flat_map(|stage| stage.hooks);
    let only = hooks.next().expect("the stage must report its one hook");
    assert!(hooks.next().is_none(), "the stage has exactly one hook");
    only
}

/// THE DEFECT. A cargo hook that spends [`HOLD`] blocked on a lock held outside
/// the run, then works for [`WORK`], overruns a [`LIMIT`] budget that started
/// when the run reached it — and is killed for somebody else's `cargo build`.
///
/// Waiting the lock out *before* spawning makes the hook's budget cover only its
/// own work: it passes, and its recorded duration is the work alone.
#[test]
fn a_hook_is_not_charged_for_a_lock_held_outside_the_run() {
    let mut env = Env::new();
    let cargo_home = TempDir::new().expect("cargo home");
    let repo = TempDir::new().expect("repo");
    let sentinel = repo.path().join("blocked");
    std::fs::write(&sentinel, b"blocked").expect("arm the sentinel");

    env.set(EnvVars::CARGO_HOME, &cargo_home.path().display().to_string());
    env.set(HOOK_TIMEOUT_ENV, &format!("{}ms", LIMIT.as_millis()));

    let holder = hold_package_cache(cargo_home.path(), &sentinel);
    let started = Instant::now();
    let outcome = run_one(repo.path(), blocked_cargo_hook(&sentinel));
    let wall = started.elapsed();
    holder.join().expect("the holder thread");

    assert_eq!(
        outcome.status,
        HookStatus::Passed,
        "a hook held up by an external lock must run, not be killed for waiting"
    );
    assert!(
        outcome.duration < LIMIT,
        "the hook's clock must start when the hook does: recorded {:?}, budget {LIMIT:?}",
        outcome.duration
    );
    assert!(
        outcome.duration >= WORK,
        "the recorded duration must still cover the work the hook did: {:?}",
        outcome.duration
    );
    assert!(
        wall >= HOLD + WORK,
        "the run really did wait out the hold before doing the work: {wall:?}"
    );
}

/// The wait is for the cargo exclusion set alone. A hook outside it is spawned
/// immediately, whatever cargo's lock is doing — making every hook queue behind
/// cargo would be a plain regression.
#[test]
fn a_hook_outside_the_cargo_set_is_never_held_back() {
    let mut env = Env::new();
    let cargo_home = TempDir::new().expect("cargo home");
    let repo = TempDir::new().expect("repo");
    let sentinel = repo.path().join("blocked");
    std::fs::write(&sentinel, b"blocked").expect("arm the sentinel");

    env.set(EnvVars::CARGO_HOME, &cargo_home.path().display().to_string());
    env.set(HOOK_TIMEOUT_ENV, &format!("{}ms", LIMIT.as_millis()));

    let holder = hold_package_cache(cargo_home.path(), &sentinel);
    let mut hook = Hook::run("fmt", "true");
    hook.always_run = true;
    hook.pass_filenames = false;
    assert!(!hook.is_cargo(), "the fixture must sit outside the cargo set");

    let started = Instant::now();
    let outcome = run_one(repo.path(), hook);
    let wall = started.elapsed();
    holder.join().expect("the holder thread");

    assert_eq!(outcome.status, HookStatus::Passed);
    assert!(
        wall < HOLD,
        "a non-cargo hook must not wait for cargo's lock: took {wall:?} against a {HOLD:?} hold"
    );
}
