//! Waiting out cargo's package-cache lock **before** a cargo hook is spawned.
//!
//! Cargo serialises its subcommands on `$CARGO_HOME/.package-cache`. Inside one
//! poly run that queue is already explicit and already honest: the `cargo`
//! exclusion set (ADR 0024) means poly's own cargo hooks never overlap, and a
//! hook waiting for its chain predecessor is not spawned at all, so its clock
//! has not started. The holder poly cannot schedule is the one **outside** the
//! run — rust-analyzer, or the author's own `cargo build` in another terminal.
//! A hook blocked there prints nothing, does no work, and is charged for the
//! wait anyway: a `cargo deny check` that takes 1.7s on its own has been killed
//! at the 30-minute whole-project budget this way.
//!
//! So poly asks the filesystem, before it spawns: is anybody holding that lock?
//! If so it waits — visibly, and under a bound of its own — and starts the
//! hook's budget only once the lock is free.
//!
//! Asking the filesystem rather than the child is the whole point. ADR 0023
//! twice rejected pausing a hook's budget when the child prints cargo's
//! `Blocking waiting for file lock` line: that line is output of the supervised
//! process, so trusting it would let any hook exempt itself from its timeout by
//! echoing the string. A probe taken before the child exists cannot be spoofed
//! by the child.
//!
//! # This is a mitigation, not a fix
//!
//! Two gaps remain open by construction, and neither is closed here:
//!
//! - **The probe/spawn race.** An external cargo can take the lock in the
//!   microseconds between [`probe`] returning [`LockState::Free`] and the child
//!   being spawned, and cargo acquires the package cache at several points in a
//!   run rather than only at startup. A hook can still be charged for a wait
//!   that begins after it started.
//! - **The artifact-directory lock is not probed.** The second lock cargo
//!   blocks on is `<target-dir>/<profile>/.cargo-lock`, and both the target
//!   directory (poly may inject `CARGO_TARGET_DIR`) and the profile would have
//!   to be guessed from a shell line. Guessing wrong is silent — no protection
//!   and no error — so it is left alone.
//!
//! For both, the post-spawn liveness notice ([`crate::supervise::LockWait`],
//! driven by the child's own output) remains the report: it names the lock and
//! says outright that the budget is still counting. What this module removes is
//! the common shape — the lock already held when the run reaches its cargo
//! hooks.
//!
//! # Why the probe cannot disturb cargo
//!
//! `flock` is advisory and scoped to an open file description, and poly is a
//! different process from the holder, so a try-then-release cannot drop or
//! corrupt anyone else's lock. The probe opens read-only and never creates the
//! file, so it needs no write access to `$CARGO_HOME` and leaves nothing behind.
//! Measured on macOS with cargo 1.97.1: a *shared* hold blocks `cargo metadata`
//! just as an exclusive one does (cargo asks exclusively, so "is there any
//! holder at all" is the right question), a non-blocking exclusive probe
//! reported the holder on 120 of 120 samples, and 2 380 probes at 400/s racing a
//! real `cargo metadata` left it exiting 0 with no blocking notice.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::consts::env_vars::EnvVars;
use crate::timeout::{Budget, LOCK_WAIT_ANNOUNCE_AFTER, LOCK_WAIT_BUDGET_DIVISOR, STILL_RUNNING_EVERY};

/// How cargo's package-cache lock is named to a reader.
///
/// Shared with the post-spawn notice, which reads this resource name out of
/// cargo's own `Blocking waiting for file lock on package cache` line, so the
/// two states name the same lock with the same words.
pub const PACKAGE_CACHE_RESOURCE: &str = "package cache";

/// The file cargo serialises its subcommands on, relative to `$CARGO_HOME`.
const PACKAGE_CACHE_LOCK_FILE: &str = ".package-cache";

/// How often the lock is re-probed while waiting.
///
/// Short enough that the hook starts promptly once the holder releases — the
/// residual wait charged to the hook is at most one interval — and far too
/// cheap, at one read-only `open`/`flock`/`close`, for the frequency to matter.
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

/// What one probe of the lock file saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    /// Nobody holds it: a cargo hook spawned now will not queue on this lock.
    Free,
    /// Somebody holds it. Since poly is not that somebody (its own cargo hooks
    /// are serialised and none is running), the holder is outside this run.
    Held,
    /// The lock could not be observed — no readable file, or a platform with no
    /// probe. Treated as [`Self::Free`] by every caller: poly must never
    /// withhold a hook because it could not see a lock.
    Unobservable,
}

/// How long, and how loudly, poly will wait for a lock it does not own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPlan {
    /// Stop waiting and spawn the hook anyway once the wait has lasted this
    /// long. An unbounded pre-spawn wait would reintroduce exactly the silent
    /// hang the timeout subsystem exists to bound.
    pub bound: Duration,
    /// Announce the wait once it has lasted this long.
    pub announce_after: Duration,
    /// Repeat that announcement at this interval.
    pub announce_every: Duration,
}

impl WaitPlan {
    /// The plan for a hook running under `budget`, or `None` when there is
    /// nothing to protect.
    ///
    /// The bound is **derived** from the hook's own budget rather than
    /// configured separately: the question "how long is it worth delaying this
    /// hook?" is already answered by how long the hook is allowed to run, and a
    /// second knob would be one more thing to set correctly before the
    /// protection works at all. Half of it, not all of it, so that a hook which
    /// waits out the full bound and then overruns is still killed inside 1.5×
    /// its configured limit — the wait is not the hook's work, so it must not be
    /// able to consume most of the run's tolerance for it.
    ///
    /// A budget with no limit returns `None`: disabling timeouts promises the
    /// pre-timeout execution path exactly — no deadline and no liveness notice —
    /// and an unbounded hook has no clock a pre-spawn wait could protect.
    #[must_use]
    pub fn for_budget(budget: Budget) -> Option<Self> {
        Some(Self {
            bound: budget.limit? / LOCK_WAIT_BUDGET_DIVISOR,
            announce_after: LOCK_WAIT_ANNOUNCE_AFTER,
            announce_every: STILL_RUNNING_EVERY,
        })
    }
}

/// How a pre-spawn wait ended. The states are kept apart because they are
/// different facts about the run: one hook never waited, one was held up and
/// then started clean, one is about to be spawned into a lock that is still
/// held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// The lock was free (or unobservable) at the first probe; nothing waited.
    NotHeld,
    /// It was held, and released after this long. The hook starts now, with its
    /// full budget and none of this time charged against it.
    Waited(Duration),
    /// Still held when [`WaitPlan::bound`] expired.
    ///
    /// The hook is spawned **anyway**. Refusing to run it would turn somebody
    /// else's `cargo build` into a failed commit, and a check that did not run
    /// must never be reported as a pass — so the deliberate choice is to run it
    /// late and let the post-spawn notice describe the rest of the wait.
    Expired(Duration),
}

/// The path of cargo's package-cache lock, or `None` when `$CARGO_HOME` cannot
/// be located.
///
/// Resolved on every call rather than cached: a hook may run with a `CARGO_HOME`
/// its stage set, and a stale answer would probe the wrong file and report a
/// free lock that is not.
#[must_use]
pub fn package_cache_lock() -> Option<PathBuf> {
    let home = EnvVars::var_os(EnvVars::CARGO_HOME)
        .map(PathBuf::from)
        .or_else(|| EnvVars::var_os(EnvVars::HOME).map(|home| PathBuf::from(home).join(".cargo")))?;
    Some(home.join(PACKAGE_CACHE_LOCK_FILE))
}

/// Ask, without blocking and without writing anything, whether `path` is
/// currently flocked.
///
/// The lock is taken and released inside this call when it is free. That window
/// is microseconds wide and a concurrent cargo would see it as a momentary
/// block; 2 380 probes racing a real `cargo metadata` never produced one.
#[cfg(unix)]
#[must_use]
pub fn probe(path: &Path) -> LockState {
    use std::os::fd::AsRawFd as _;

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // No lock file is not an error and not a reason to wait: cargo creates
        // it on first use, so its absence means nothing can be holding it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return LockState::Free,
        Err(_) => return LockState::Unobservable,
    };
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is owned by `file`, which outlives both calls; `flock` has no
    // other precondition and ignores the open mode, so a read-only descriptor is
    // enough to ask the question.
    let taken = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if taken == 0 {
        // Released explicitly rather than by dropping `file`, to keep poly's own
        // hold as short as the syscall pair allows.
        // SAFETY: as above.
        unsafe { libc::flock(fd, libc::LOCK_UN) };
        return LockState::Free;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EWOULDBLOCK) => LockState::Held,
        _ => LockState::Unobservable,
    }
}

/// Non-Unix builds do not probe: `flock` is the mechanism cargo uses on Unix,
/// and reporting [`LockState::Unobservable`] keeps every caller on the path it
/// takes when there is nothing to see.
#[cfg(not(unix))]
#[must_use]
pub fn probe(_path: &Path) -> LockState {
    LockState::Unobservable
}

/// Wait for `path` to be unlocked, up to `plan.bound`, announcing the wait on
/// `plan`'s cadence.
///
/// `notify` receives the time waited so far, and is called only while the lock
/// is genuinely held — a hook that was never delayed says nothing.
pub fn wait_until_free(path: &Path, plan: WaitPlan, notify: &dyn Fn(Duration)) -> Wait {
    if probe(path) != LockState::Held {
        return Wait::NotHeld;
    }
    let started = Instant::now();
    let mut next_notice = started + plan.announce_after;
    loop {
        let waited = started.elapsed();
        if waited >= plan.bound {
            return Wait::Expired(waited);
        }
        if Instant::now() >= next_notice {
            notify(waited);
            next_notice = Instant::now() + plan.announce_every;
        }
        std::thread::sleep(PROBE_INTERVAL);
        if probe(path) != LockState::Held {
            return Wait::Waited(started.elapsed());
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{LockState, PACKAGE_CACHE_RESOURCE, Wait, WaitPlan, package_cache_lock, probe, wait_until_free};
    use crate::timeout::{Budget, DEFAULT_WORKSPACE_HOOK_TIMEOUT, LOCK_WAIT_ANNOUNCE_AFTER, STILL_RUNNING_EVERY};
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A real `flock` held on `path` by a background thread until [`Self::drop`].
    ///
    /// A held lock is the thing under test, so it is a real one: `flock` is
    /// per-open-file-description, so a second open in this same process
    /// contends exactly as another process would.
    struct Holder {
        release: Option<mpsc::Sender<()>>,
        joined: Option<std::thread::JoinHandle<()>>,
    }

    impl Holder {
        fn take(path: &Path) -> Self {
            let path = path.to_path_buf();
            let (release, released) = mpsc::channel();
            let (ready, holding) = mpsc::channel();
            let joined = std::thread::spawn(move || {
                let mut file = std::fs::File::create(&path).expect("create lock file");
                file.write_all(b"held").expect("write lock file");
                // SAFETY: the descriptor is owned by `file` for the whole block.
                let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
                assert_eq!(taken, 0, "the holder must actually acquire the lock");
                ready.send(()).expect("signal that the lock is held");
                let _ = released.recv();
            });
            holding.recv().expect("wait until the lock is held");
            Self {
                release: Some(release),
                joined: Some(joined),
            }
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            drop(self.release.take());
            if let Some(joined) = self.joined.take() {
                let _ = joined.join();
            }
        }
    }

    fn lock_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".package-cache")
    }

    /// The state `path` settles on, re-probing for up to a second.
    ///
    /// A release is not necessarily visible to the next `flock` on another
    /// thread the instant the holder's descriptor closes, and under a loaded
    /// test binary that gap is observable. Polling for the settled state keeps
    /// the assertion exact — a probe stuck on one answer never reaches the other
    /// one — without pinning a timing the kernel does not promise.
    fn settled(path: &Path) -> LockState {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let state = probe(path);
            if state == LockState::Free || Instant::now() >= deadline {
                return state;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A plan whose cadence is measured in milliseconds, so a test can observe
    /// a bound expiring and a notice firing without sleeping for seconds.
    fn plan(bound_ms: u64, announce_ms: u64) -> WaitPlan {
        WaitPlan {
            bound: Duration::from_millis(bound_ms),
            announce_after: Duration::from_millis(announce_ms),
            announce_every: Duration::from_millis(announce_ms),
        }
    }

    #[test]
    fn a_real_holder_is_seen_and_the_release_is_seen_too() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        let holder = Holder::take(&path);
        assert_eq!(probe(&path), LockState::Held);
        drop(holder);
        assert_eq!(settled(&path), LockState::Free);
    }

    #[test]
    fn a_missing_lock_file_reads_as_free_and_is_not_created() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        assert_eq!(probe(&path), LockState::Free);
        assert!(!path.exists(), "the probe must never create the lock file");
    }

    #[test]
    fn nothing_is_waited_for_when_the_lock_is_free() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = lock_path(&dir);

        let waited = wait_until_free(&path, plan(500, 10), &|_| {
            panic!("must not announce a wait that never happened")
        });
        assert_eq!(waited, Wait::NotHeld);
    }

    #[test]
    fn a_held_lock_is_waited_out_and_reported_while_it_is_held() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = lock_path(&dir);
        let holder = Holder::take(&path);

        let hold = Duration::from_millis(400);
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            drop(holder);
        });

        let notices = std::sync::Mutex::new(Vec::new());
        let started = Instant::now();
        let waited = wait_until_free(&path, plan(5_000, 50), &|elapsed| {
            notices.lock().expect("notices").push(elapsed);
        });

        let Wait::Waited(held_for) = waited else {
            panic!("the wait must report that the lock was held: {waited:?}");
        };
        assert!(
            held_for >= hold,
            "waited {held_for:?}, but the lock was held for {hold:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait must end at the release"
        );
        let notices = notices.into_inner().expect("notices");
        assert!(
            !notices.is_empty(),
            "a wait past the announce threshold must be reported"
        );
        assert!(
            notices.iter().all(|elapsed| *elapsed >= Duration::from_millis(50)),
            "every notice reports the time waited so far: {notices:?}"
        );
    }

    /// An unbounded pre-spawn wait would be the same silent hang the timeout
    /// subsystem exists to prevent, one layer earlier — so the wait is run on
    /// its own thread and the *test* is what times out. A hanging assertion
    /// would hang the suite instead of reporting the defect.
    #[test]
    fn the_bound_expires_rather_than_waiting_forever() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = lock_path(&dir);
        let holder = Holder::take(&path);

        let bound = Duration::from_millis(300);
        let (done, ended) = mpsc::channel();
        let waiting = {
            let path = path.clone();
            std::thread::spawn(move || {
                let _ = done.send(wait_until_free(&path, plan(300, 50), &|_| {}));
            })
        };

        let waited = ended
            .recv_timeout(Duration::from_secs(5))
            .expect("the wait must end at its bound, not run until the holder gives up");
        waiting.join().expect("the waiting thread");
        drop(holder);

        let Wait::Expired(gave_up_after) = waited else {
            panic!("a lock nobody releases must expire the bound: {waited:?}");
        };
        assert!(
            gave_up_after >= bound,
            "gave up after {gave_up_after:?}, bound {bound:?}"
        );
        assert!(
            gave_up_after < Duration::from_secs(1),
            "the bound must end the wait promptly: {gave_up_after:?}"
        );
    }

    #[test]
    fn the_bound_is_half_the_hook_budget_and_an_unbounded_budget_never_waits() {
        let plan = WaitPlan::for_budget(Budget::bounded(DEFAULT_WORKSPACE_HOOK_TIMEOUT)).expect("a bounded budget");
        assert_eq!(plan.bound, DEFAULT_WORKSPACE_HOOK_TIMEOUT / 2);
        assert_eq!(plan.announce_after, LOCK_WAIT_ANNOUNCE_AFTER);
        assert_eq!(plan.announce_every, STILL_RUNNING_EVERY);

        assert_eq!(
            WaitPlan::for_budget(Budget::unlimited()),
            None,
            "an unbounded budget has no clock to protect, so poly must not delay the hook"
        );
    }

    #[test]
    fn the_lock_path_follows_cargo_home() {
        let home = package_cache_lock().expect("a cargo home");
        assert_eq!(
            home.file_name().and_then(std::ffi::OsStr::to_str),
            Some(".package-cache")
        );
        assert_eq!(PACKAGE_CACHE_RESOURCE, "package cache");
    }
}
