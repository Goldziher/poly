//! Run a child process under a deadline, and make the kill stick.
//!
//! [`run`] is [`std::process::Command::output`] with three additions a hook
//! runner needs and the standard library does not offer:
//!
//! 1. **A deadline.** The child is killed once its [`Budget::limit`] elapses,
//!    so a wedged hook cannot hang a commit indefinitely.
//! 2. **A liveness signal.** While the child runs, `notify` is called at the
//!    budget's announce cadence, so a slow or hung hook names itself on screen
//!    instead of leaving the terminal silent. The notice carries *why* the child
//!    is quiet when the child has said so: a cargo subcommand queued behind a
//!    lock held outside the run prints one `Blocking waiting for file lock` line
//!    and then nothing, which is otherwise indistinguishable from wedged
//!    ([`LockWait`]).
//! 3. **A kill that terminates the process *tree*.** On Unix the child is put
//!    in its own process group and the group is signalled — `SIGTERM` first so
//!    a well-behaved tool can release its locks, `SIGKILL` after a short grace.
//!    On Windows the child is put in a Job Object and the job is terminated,
//!    which reaches the same tree. Killing only the direct `sh -c` / `cmd /C`
//!    would leave the real tool orphaned and still holding whatever the hang
//!    was holding, which is strictly worse than the hang.
//!
//! A fourth thing it does is *not* over-report: the deadline path is entered
//! from a `try_wait` taken microseconds earlier, so poly deciding to kill a
//! child is never taken as proof that the kill is what ended it. A hook that
//! finishes on its own in that window is reported on its own exit status —
//! failing a hook that passed is a worse defect than the hang this module
//! exists to bound.
//!
//! Trade-off of the process group: children no longer share poly's group, so a
//! terminal `Ctrl-C` reaches poly but not them. poly kills the group itself on
//! timeout and on a launch/wait error; a `Ctrl-C` during a long hook is the one
//! path that can still orphan a child, and closing it needs a signal handler in
//! the binary (`cleanup::cleanup` is registered but not yet wired). Setting the
//! timeout to `0`/`off` restores the previous, group-sharing behaviour.
//!
//! Output is drained on dedicated threads. `Command::output` does this
//! internally; doing it by hand is what makes a bounded wait possible, and it
//! is not optional — a child that fills the 64 KiB pipe buffer while nobody
//! reads it blocks forever, which would reintroduce the hang through the back
//! door.
//!
//! What the drain threads read is also handed onward *while the child runs*
//! ([`run_streaming`]), which is what makes the progress preview a preview: fed
//! only after exit, a long hook shows an empty window for its whole run and then
//! everything at once — the same "is it working or is it wedged?" question the
//! liveness notice exists to answer. The hand-off deliberately happens on the
//! **supervising thread**, not on the drain threads; the `output` submodule owns
//! that machinery and says why.

mod output;

use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(not(unix))]
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject};

use output::Live;
pub use output::LockWait;

use crate::timeout::Budget;

/// How often the supervisor checks whether the child has exited. Matches the
/// existing bounded-wait probe elsewhere in the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a killed process group is given to exit on `SIGTERM` before it is
/// sent `SIGKILL`. On Windows, how long the Job Object is given to die before
/// the child is killed directly.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// The exit code Windows records for each process poly terminates as part of a
/// wedged hook's tree. It is never the reported verdict — `timed_out` is — so it
/// only has to be non-zero for anything that inspects a killed grandchild, and
/// `1` is what `Child::kill` already uses.
#[cfg(not(unix))]
const TREE_KILL_EXIT_CODE: u32 = 1;

/// The result of a supervised run.
#[derive(Debug)]
pub struct Supervised {
    /// The child's exit status and captured output.
    pub output: Output,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
    /// Whether poly killed the process for overrunning its budget. When set,
    /// `output.status` describes the *kill*, not the tool's own verdict.
    ///
    /// Set only when the kill is what actually ended the child, which is not the
    /// same as poly having decided to kill it: a child that exits on its own in
    /// the moment between the supervisor's last look and the signal reports its
    /// own status with this clear. Reporting that one as killed would fail a
    /// hook that passed.
    pub timed_out: bool,
}

impl Supervised {
    /// Wrap an already-completed [`Output`] that ran without supervision.
    #[must_use]
    pub fn completed(output: Output, elapsed: Duration) -> Self {
        Self {
            output,
            elapsed,
            timed_out: false,
        }
    }
}

/// Spawn `command` and wait for it under `budget`, killing it if it overruns,
/// discarding its output as it arrives.
///
/// `notify` is called on the supervising thread each time the announce cadence
/// comes due, with the elapsed time and — when the child's last output said it
/// is queued behind a lock — the resource it is waiting for.
///
/// # Errors
///
/// Returns the spawn or wait error. The child (and its group) is killed before
/// a wait error propagates, so no supervised process outlives this call.
pub fn run(
    command: &mut Command,
    budget: Budget,
    notify: &dyn Fn(Duration, Option<&str>),
) -> std::io::Result<Supervised> {
    run_streaming(command, budget, &mut |_| {}, notify)
}

/// [`run`], plus every byte the child produces handed to `stream` as it arrives.
///
/// `stream` is called only from the supervising thread — never from a drain
/// thread — so an implementation needs no synchronisation of its own, and one
/// that blocks (drawing to a terminal, say) cannot stall a drain and fill the
/// child's pipe. That is enforced by this signature and not merely intended:
/// `stream` carries no `Send` bound, so it cannot be handed to a drain thread
/// without changing the contract here first. It is called at most once per pipe
/// per [`POLL_INTERVAL`], with everything that arrived since the last call, so a
/// torrential child costs the sink a bounded number of calls rather than one per
/// read.
///
/// The bytes handed to `stream` are exactly the bytes of
/// [`Supervised::output`] — each stream's own order preserved, nothing dropped
/// and nothing repeated — interleaved between the two pipes in arrival order.
///
/// # Errors
///
/// As [`run`].
pub fn run_streaming(
    command: &mut Command,
    budget: Budget,
    stream: &mut dyn FnMut(&[u8]),
    notify: &dyn Fn(Duration, Option<&str>),
) -> std::io::Result<Supervised> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(command);

    let started = Instant::now();
    let mut child = command.spawn()?;
    let tree = ProcessTree::of(&child);
    let lock_wait = Arc::new(LockWait::default());
    let mut live = Live::start(child.stdout.take(), child.stderr.take(), &lock_wait, stream);

    let waited = wait_within(&mut child, &tree, budget, started, &lock_wait, &mut live, notify);
    if waited.is_err() {
        // Nothing left to report the status to; the kill is for the child's
        // benefit, not the caller's.
        let _ = terminate(&mut child, &tree);
    }
    let (status, timed_out) = waited?;
    let elapsed = started.elapsed();
    let (stdout, stderr) = live.finish();

    Ok(Supervised {
        output: Output { status, stdout, stderr },
        elapsed,
        timed_out,
    })
}

/// Wait for `child`, announcing and killing per `budget`. The `bool` is set
/// when the budget — not the child — ended the wait.
///
/// "The budget ended it" is decided from what actually killed the child, never
/// from having entered the kill path: the deadline is checked against a
/// `try_wait` taken microseconds earlier, and a child that exits in that gap
/// would otherwise be reported as killed on the strength of a signal that ended
/// nothing.
fn wait_within(
    child: &mut Child,
    tree: &ProcessTree,
    budget: Budget,
    started: Instant,
    lock_wait: &LockWait,
    live: &mut Live<'_>,
    notify: &dyn Fn(Duration, Option<&str>),
) -> std::io::Result<(ExitStatus, bool)> {
    let mut next_notice = budget.announce_after.map(|after| started + after);
    loop {
        // Ahead of the exit check, so a child that ends between two polls has
        // its last words handed on by the same pass that would have seen them
        // had it lived. Whatever is left after the pipes close is flushed by
        // `Live::finish`.
        live.pump();
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        let now = Instant::now();
        if budget.limit.is_some_and(|limit| now.duration_since(started) >= limit) {
            if let Terminated::AlreadyExited(status) = terminate(child, tree) {
                return Ok((status, false));
            }
            let status = child.wait()?;
            return Ok((status, killed_by_signal(status)));
        }
        if let Some(due) = next_notice {
            if now >= due {
                notify(now.duration_since(started), lock_wait.waiting_on().as_deref());
                next_notice = Some(now + budget.announce_every);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Put the child in its own process group so the whole tree can be signalled.
#[cfg(unix)]
fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
}

/// Windows has no process group to ask for at spawn time; the tree is collected
/// after the fact, by [`ProcessTree::of`].
#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

/// The handle on everything the child spawns, by which the kill is made to reach
/// the whole tree rather than just the process poly launched.
///
/// A hook is a shell line: killing the `sh -c` / `cmd /C` alone leaves the real
/// tool — the compiler, the test runner, whatever was actually wedged — running
/// and still holding whatever the hang was holding, which is worse than the
/// hang. Unix gets this at spawn time from the process group
/// ([`isolate_process_group`]), so there is nothing to carry; Windows has to
/// create a Job Object and hold its handle until the kill.
#[cfg(unix)]
struct ProcessTree;

#[cfg(unix)]
impl ProcessTree {
    /// Nothing to collect: the group was requested before the child existed.
    fn of(_child: &Child) -> Self {
        Self
    }
}

/// The Job Object the child was placed in, when one could be created.
///
/// `None` means the job could not be created or the child could not be assigned
/// to it — a hardened CI agent may already hold the process in a job that
/// forbids nesting. That is not an error: the kill then reaches only the process
/// poly launched, which is exactly what it did before there was a job at all.
#[cfg(not(unix))]
struct ProcessTree(Option<Job>);

#[cfg(not(unix))]
impl ProcessTree {
    /// Put `child` in a fresh Job Object, so terminating the job terminates
    /// every process it went on to spawn.
    ///
    /// The job is deliberately created with **no limits** — in particular not
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. It is a handle for the kill path,
    /// not a lifetime: closing it at the end of a normal run must leave a
    /// daemon the hook legitimately started (a language server, an sccache
    /// server) running, exactly as the Unix process group does.
    ///
    /// The assignment happens just after the child is created rather than
    /// before it runs, which leaves a window in which a very fast hook could
    /// spawn a grandchild that escapes the job. Closing it needs
    /// `CREATE_SUSPENDED` plus `ResumeThread`, and the standard library does not
    /// expose the child's main thread handle to resume — so the window stays,
    /// and the kill still reaches everything spawned after it.
    fn of(child: &Child) -> Self {
        use std::os::windows::io::AsRawHandle as _;

        // SAFETY: both are plain kernel32 calls. `CreateJobObjectW` takes null
        // attributes (default security descriptor) and a null name (an unnamed,
        // private job); it returns a null handle on failure, which is checked
        // before use. The process handle is the live one owned by `child`,
        // borrowed for the duration of the call.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Self(None);
        }
        let job = Job(job);
        // SAFETY: as above; `job.0` is a valid job handle and `child` outlives
        // the call.
        let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle().cast()) };
        if assigned == 0 { Self(None) } else { Self(Some(job)) }
    }

    /// Terminate every process in the job; `false` when there was no job to
    /// terminate, or the call failed and the caller still has to kill the child
    /// it launched.
    fn terminate(&self) -> bool {
        let Some(job) = &self.0 else {
            return false;
        };
        // SAFETY: `job.0` is the handle `CreateJobObjectW` returned and has not
        // been closed — `Job` owns it and closes it only on drop.
        unsafe { TerminateJobObject(job.0, TREE_KILL_EXIT_CODE) != 0 }
    }
}

/// An owned Job Object handle, closed exactly once.
#[cfg(not(unix))]
struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(not(unix))]
impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `CreateJobObjectW`, is non-null (checked at
        // construction), and `Drop` runs once. The job carries no
        // kill-on-close limit, so closing it does not touch its members.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// What poly found when it went to kill the child.
///
/// Entering the kill path is not evidence that the kill is what ended the run,
/// and the difference decides whether a hook is reported as killed or as having
/// passed — so [`terminate`] answers the question instead of leaving the caller
/// to assume.
enum Terminated {
    /// The child had already exited before anything was signalled. The status is
    /// its own, and no signal was sent: the pid was a zombie, and `SIGTERM` to
    /// its group would have changed nothing.
    AlreadyExited(ExitStatus),
    /// poly signalled the child (or its group). On Unix that still does not
    /// prove the signal is what ended it — see [`killed_by_signal`].
    Signalled,
}

/// Kill the child's process group: `SIGTERM`, then `SIGKILL` after a grace
/// period. Returns once nothing in the group is alive.
///
/// The first thing it does is look again. The caller decided to kill from a
/// `try_wait` a few instructions ago, and a child that exited in between must
/// not be counted as killed — the signal would land on a zombie, and the status
/// waiting to be reaped is the tool's own verdict, not poly's. That covers the
/// child that beat the decision; [`killed_by_signal`] covers the one that beats
/// the signal itself.
#[cfg(unix)]
fn terminate(child: &mut Child, _tree: &ProcessTree) -> Terminated {
    if let Ok(Some(status)) = child.try_wait() {
        return Terminated::AlreadyExited(status);
    }
    let Ok(group) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Terminated::Signalled;
    };
    // SAFETY: `kill` is a plain libc call with no memory operands. The negative
    // pid targets the process group whose leader is `group` — the group created
    // for this child by `isolate_process_group`, so nothing outside this hook's
    // own process tree can be signalled.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    if exited_within(child, TERM_GRACE) {
        return Terminated::Signalled;
    }
    // SAFETY: as above; the group could not be talked into exiting.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
    Terminated::Signalled
}

/// Kill the child's Job Object — every process it spawned along with it — and
/// fall back to killing the process poly launched if there is no job or the job
/// will not die.
///
/// The escalation mirrors the Unix one: the tree-wide kill first, a grace period
/// for it to take effect, then the blunt instrument on whatever is left. Windows
/// has no polite signal to precede it with, so the grace covers only the delay
/// between asking for the job's death and the child being reapable.
#[cfg(not(unix))]
fn terminate(child: &mut Child, tree: &ProcessTree) -> Terminated {
    if let Ok(Some(status)) = child.try_wait() {
        return Terminated::AlreadyExited(status);
    }
    if tree.terminate() && exited_within(child, TERM_GRACE) {
        return Terminated::Signalled;
    }
    let _ = child.kill();
    let _ = child.wait();
    Terminated::Signalled
}

/// Whether `status` shows the child was ended by a signal — the only evidence
/// there is that poly's kill, rather than the child itself, ended the run.
///
/// This is what carries the invariant, and the re-probe in [`terminate`] is not
/// enough on its own: hammering the boundary shows ~1% of children still alive
/// at *both* looks and yet reaped with a plain `exit(0)`, because a child
/// already inside its own exit path finishes it before the `SIGTERM` can land.
/// A status carrying no signal proves the kill never landed — whatever the clock
/// said, the child ran to its own exit, and its own code is the verdict.
#[cfg(unix)]
fn killed_by_signal(status: ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;

    status.signal().is_some()
}

/// Windows records no cause of death — a terminated process's exit code is
/// indistinguishable from one it chose for itself — so a kill that was issued is
/// taken at its word. The re-probe in [`terminate`] is the whole defence there.
#[cfg(not(unix))]
fn killed_by_signal(_status: ExitStatus) -> bool {
    true
}

/// Poll for the child's exit for at most `grace`; `true` when it exited.
fn exited_within(child: &mut Child, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(_) => return false,
        }
    }
    false
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::num::NonZero;
    use std::process::Command;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::{Duration, Instant};

    use super::{run, run_streaming};
    use crate::timeout::Budget;

    fn budget(limit: Option<Duration>) -> Budget {
        Budget {
            limit,
            announce_after: None,
            announce_every: Duration::from_mins(1),
        }
    }

    fn sh(line: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(line);
        command
    }

    /// A child that does nothing but succeed, spawned exactly the way a hook
    /// body is. Its whole lifetime is shell startup, which is what puts its exit
    /// in the same handful of microseconds as the supervisor's deadline check.
    fn exits_immediately() -> Command {
        sh("exit 0")
    }

    #[test]
    fn a_child_that_finishes_in_time_reports_its_own_output_and_status() {
        let supervised = run(
            &mut sh("printf OUT; printf ERR >&2; exit 3"),
            budget(Some(Duration::from_secs(30))),
            &|_, _| {},
        )
        .expect("supervised run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.status.code(), Some(3));
        assert_eq!(supervised.output.stdout, b"OUT");
        assert_eq!(supervised.output.stderr, b"ERR");
    }

    #[test]
    fn a_child_that_overruns_is_killed_and_flagged() {
        let supervised = run(
            &mut sh("sleep 30"),
            budget(Some(Duration::from_millis(150))),
            &|_, _| {},
        )
        .expect("run");

        assert!(supervised.timed_out, "the overrun must be reported as such");
        assert!(supervised.elapsed >= Duration::from_millis(150));
        assert!(
            supervised.elapsed < Duration::from_secs(30),
            "the supervisor must not have waited for the child"
        );
    }

    #[test]
    fn a_still_running_child_announces_itself_before_it_is_killed() {
        let seen: Mutex<Vec<Duration>> = Mutex::new(Vec::new());
        let announce = Budget {
            limit: Some(Duration::from_millis(400)),
            announce_after: Some(Duration::from_millis(50)),
            announce_every: Duration::from_millis(50),
        };
        let supervised = run(&mut sh("sleep 30"), announce, &|elapsed, _| {
            seen.lock().expect("notice lock").push(elapsed);
        })
        .expect("run");

        assert!(supervised.timed_out);
        let notices = seen.into_inner().expect("notices");
        assert!(
            notices.len() >= 2,
            "a hung child must keep announcing itself: {notices:?}"
        );
        assert!(notices[0] >= Duration::from_millis(50));
    }

    /// THE CONFLATION. A cargo subcommand queued behind a lock held outside the
    /// run prints its notice and then goes silent for the whole window — exactly
    /// what a wedged tool looks like. The liveness callback must be told which
    /// one it is watching.
    #[test]
    fn a_child_queued_behind_a_cargo_lock_reports_what_it_is_waiting_for() {
        let seen: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());
        let announce = Budget {
            limit: Some(Duration::from_millis(600)),
            announce_after: Some(Duration::from_millis(150)),
            announce_every: Duration::from_millis(100),
        };
        let supervised = run(
            &mut sh("printf '    Blocking waiting for file lock on package cache\\n' >&2; sleep 30"),
            announce,
            &|_, waiting_on| {
                seen.lock().expect("notice lock").push(waiting_on.map(str::to_owned));
            },
        )
        .expect("run");

        assert!(supervised.timed_out);
        let notices = seen.into_inner().expect("notices");
        assert!(!notices.is_empty(), "a silent child must still announce itself");
        assert!(
            notices.iter().all(|notice| notice.as_deref() == Some("package cache")),
            "every notice must name the lock the child is queued on: {notices:?}"
        );
    }

    /// …and it must stop saying so the moment the lock is granted, or the notice
    /// becomes a different false report.
    #[test]
    fn output_after_the_lock_notice_clears_the_wait() {
        let seen: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());
        let announce = Budget {
            limit: Some(Duration::from_millis(800)),
            announce_after: Some(Duration::from_millis(100)),
            announce_every: Duration::from_millis(100),
        };
        let supervised = run(
            &mut sh(
                "printf '    Blocking waiting for file lock on build directory\\n' >&2; sleep 0.35; \
                 printf '    Checking poly-hooks v0.1.0\\n' >&2; sleep 30",
            ),
            announce,
            &|_, waiting_on| {
                seen.lock().expect("notice lock").push(waiting_on.map(str::to_owned));
            },
        )
        .expect("run");

        assert!(supervised.timed_out);
        let notices = seen.into_inner().expect("notices");
        assert_eq!(
            notices.first().map(Option::as_deref),
            Some(Some("build directory")),
            "the wait must be reported while it is happening: {notices:?}"
        );
        assert_eq!(
            notices.last().map(Option::as_deref),
            Some(None),
            "once the tool resumes printing it is working, not queued: {notices:?}"
        );
    }

    #[test]
    fn a_child_that_says_nothing_is_never_reported_as_waiting_on_a_lock() {
        let seen: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());
        let announce = Budget {
            limit: Some(Duration::from_millis(400)),
            announce_after: Some(Duration::from_millis(100)),
            announce_every: Duration::from_millis(100),
        };
        run(&mut sh("sleep 30"), announce, &|_, waiting_on| {
            seen.lock().expect("notice lock").push(waiting_on.map(str::to_owned));
        })
        .expect("run");

        let notices = seen.into_inner().expect("notices");
        assert!(!notices.is_empty());
        assert!(
            notices.iter().all(Option::is_none),
            "a genuinely wedged hook must not be excused as queued: {notices:?}"
        );
    }

    /// THE BOUNDARY RACE. The supervisor used to report a timeout from having
    /// *entered* the deadline path rather than from what actually ended the
    /// child, so a child that exited 0 between the supervisor's last look and
    /// the signal actually landing was reported as killed: poly telling an
    /// author their hook was killed when it passed, and blocking the commit on
    /// it.
    ///
    /// The window is microseconds wide and cannot be staged, so it is hammered —
    /// thousands of children that exit 0 against a budget already spent by the
    /// time the supervisor first looks, run heavily oversubscribed so the
    /// supervising thread can be descheduled inside the window. Against the
    /// unfixed supervisor this observed 52–117 false reports per run over five
    /// consecutive runs (0.6–1.4% of 8192). The assertion is the invariant, not
    /// the rate: one false report is the whole defect.
    ///
    /// It is also what shows re-probing before the signal is not sufficient by
    /// itself — with the re-probe in and [`super::killed_by_signal`] stubbed
    /// out, this still reports 109–144 false failures per run.
    #[test]
    fn a_child_that_exits_on_its_own_is_never_reported_as_timed_out() {
        /// Enough spawns that the window is sampled many times over; the
        /// measured hit rate leaves the unfixed supervisor no realistic way to
        /// pass this.
        const RUNS: usize = 8192;
        /// Oversubscription factor. The window only opens wide when the
        /// supervising thread can lose the CPU between its probe and its kill,
        /// which needs far more runnable threads than cores.
        const OVERSUBSCRIBE: usize = 8;
        /// A budget already spent by the time the child is spawned, so every run
        /// takes the deadline path on its first probe — right where the child's
        /// own exit lands.
        const SPENT: Duration = Duration::from_micros(500);

        let workers = std::thread::available_parallelism().map_or(4, NonZero::get) * OVERSUBSCRIBE;
        let per_worker = RUNS.div_ceil(workers);
        let (false_reports, own_exit, killed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));

        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    for _ in 0..per_worker {
                        let supervised = run(&mut exits_immediately(), budget(Some(SPENT)), &|_, _| {}).expect("run");
                        match (supervised.output.status.code(), supervised.timed_out) {
                            (Some(0), true) => false_reports.fetch_add(1, Relaxed),
                            (Some(0), false) => own_exit.fetch_add(1, Relaxed),
                            _ => killed.fetch_add(1, Relaxed),
                        };
                    }
                });
            }
        });

        let (own_exit, killed) = (own_exit.into_inner(), killed.into_inner());
        assert!(
            own_exit > 0 && killed > 0,
            "the hammer has to straddle the deadline to sample the window at all — all runs landed on one \
             side, so this proves nothing: exited-on-own={own_exit} killed={killed}"
        );
        assert_eq!(
            false_reports.into_inner(),
            0,
            "a child that exited 0 was reported as killed: the run entered the deadline path, but the child \
             ended itself and its own status is the verdict ({own_exit} exited on their own, {killed} were killed)"
        );
    }

    /// The same invariant with the timing taken out of it: a child that outlives
    /// `SIGTERM`, finishes its work and exits 0 was ended by itself, not by
    /// poly's kill. Deterministic where the hammer above is statistical.
    #[test]
    fn a_child_that_outlives_the_signal_and_exits_zero_reports_its_own_status() {
        let supervised = run(
            &mut sh("trap '' TERM; sleep 0.2; exit 0"),
            budget(Some(Duration::from_millis(50))),
            &|_, _| {},
        )
        .expect("run");

        assert_eq!(supervised.output.status.code(), Some(0));
        assert!(
            !supervised.timed_out,
            "the kill did not land and the child exited 0 on its own — reporting a timeout would discard \
             the tool's own verdict"
        );
    }

    /// Streaming may not cost the caller a byte. Both pipes are driven well past
    /// the pipe buffer at once, so the streamed bytes are interleaved; splitting
    /// them back apart must reproduce each captured stream exactly — anything
    /// dropped, repeated, or reordered *within* a stream shows up here.
    #[test]
    fn streamed_bytes_are_exactly_the_captured_bytes_once_each() {
        /// 40 bytes doubled to 1000 in the shell, then written this many times
        /// per stream — comfortably past any platform's pipe buffer.
        const WRITES: usize = 200;
        const PAYLOAD: usize = 1000;

        let mut streamed = Vec::new();
        let supervised = run_streaming(
            &mut sh(
                "a=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; a=$a$a$a$a$a; a=$a$a$a$a$a; \
                 b=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; b=$b$b$b$b$b; b=$b$b$b$b$b; \
                 i=0; while [ $i -lt 200 ]; do printf '%s' \"$a\"; printf '%s' \"$b\" >&2; i=$((i+1)); done",
            ),
            budget(Some(Duration::from_secs(30))),
            &mut |chunk| streamed.extend_from_slice(chunk),
            &|_, _| {},
        )
        .expect("run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.stdout.len(), WRITES * PAYLOAD);
        assert_eq!(supervised.output.stderr.len(), WRITES * PAYLOAD);
        let (out, err): (Vec<u8>, Vec<u8>) = streamed.iter().copied().partition(|&byte| byte == b'a');
        assert_eq!(out, supervised.output.stdout, "stdout was not streamed verbatim");
        assert_eq!(err, supervised.output.stderr, "stderr was not streamed verbatim");
        assert_eq!(
            streamed.len(),
            supervised.output.stdout.len() + supervised.output.stderr.len(),
            "the sink saw bytes the caller did not, or the same bytes twice"
        );
    }

    /// A hook that goes on to wedge is the case the preview matters most for:
    /// what it printed before it stopped is the only clue about where it stuck,
    /// and it is worthless if it only appears once poly has killed it. So the
    /// bytes must reach the sink while the child is still alive, not merely by
    /// the time the call returns.
    ///
    /// The child prints immediately and then blocks for the whole budget, so the
    /// first chunk is due within a poll or two of the spawn — a post-mortem sink
    /// cannot see it before the kill at 600 ms.
    #[test]
    fn a_wedged_child_reaches_the_sink_before_it_is_killed() {
        const LIMIT: Duration = Duration::from_millis(600);
        /// 20× the poll interval, and less than half the budget: comfortably
        /// after a live sink's first chunk, comfortably before a post-mortem
        /// one's.
        const WHILE_ALIVE: Duration = Duration::from_millis(200);

        let started = Instant::now();
        let (mut streamed, mut first_at) = (Vec::new(), None);
        let supervised = run_streaming(
            &mut sh("printf 'BEFORE\\n'; sleep 30"),
            budget(Some(LIMIT)),
            &mut |chunk| {
                first_at.get_or_insert_with(|| started.elapsed());
                streamed.extend_from_slice(chunk);
            },
            &|_, _| {},
        )
        .expect("run");

        assert!(supervised.timed_out);
        assert_eq!(streamed, b"BEFORE\n");
        assert_eq!(supervised.output.stdout, b"BEFORE\n");
        assert!(supervised.elapsed >= LIMIT);
        let first_at = first_at.expect("the sink was never called");
        assert!(
            first_at < WHILE_ALIVE,
            "the child's output only reached the sink after it was killed, at {first_at:?}"
        );
    }

    /// The tail of a hook's output can arrive after the process poly waited on
    /// has already gone: a `sh -c` line that backgrounds a writer exits at once
    /// while the writer keeps the pipe open. Those bytes are captured — they are
    /// in the reported output either way — so the sink has to be given them too,
    /// or the preview and the report disagree about what the hook said.
    #[test]
    fn output_arriving_after_the_child_exits_still_reaches_the_sink() {
        let mut streamed = Vec::new();
        let supervised = run_streaming(
            &mut sh("{ sleep 0.3; printf 'LATE\\n'; } & printf 'EARLY\\n'; exit 0"),
            budget(Some(Duration::from_secs(30))),
            &mut |chunk| streamed.extend_from_slice(chunk),
            &|_, _| {},
        )
        .expect("run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.stdout, b"EARLY\nLATE\n");
        assert_eq!(
            streamed, b"EARLY\nLATE\n",
            "the sink lost what arrived after the supervised process exited"
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let supervised = run(
            &mut sh(
                "i=0; while [ $i -lt 4000 ]; do printf '0123456789012345678901234567890123456789\\n'; \
                     i=$((i+1)); done",
            ),
            budget(Some(Duration::from_secs(30))),
            &|_, _| {},
        )
        .expect("run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.stdout.len(), 4000 * 41);
    }
}
