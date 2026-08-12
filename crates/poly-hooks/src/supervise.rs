//! Run a child process under a deadline, and make the kill stick.
//!
//! [`run`] is [`std::process::Command::output`] with three additions a hook
//! runner needs and the standard library does not offer:
//!
//! 1. **A deadline.** The child is killed once its [`Budget::limit`] elapses,
//!    so a wedged hook cannot hang a commit indefinitely.
//! 2. **A liveness signal.** While the child runs, `notify` is called at the
//!    budget's announce cadence, so a slow or hung hook names itself on screen
//!    instead of leaving the terminal silent.
//! 3. **A kill that terminates the process *tree*.** On Unix the child is put
//!    in its own process group and the group is signalled — `SIGTERM` first so
//!    a well-behaved tool can release its locks, `SIGKILL` after a short grace.
//!    Killing only the direct `sh -c` would leave the real tool orphaned and
//!    still holding whatever the hang was holding, which is strictly worse than
//!    the hang.
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

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::timeout::Budget;

/// How often the supervisor checks whether the child has exited. Matches the
/// existing bounded-wait probe elsewhere in the workspace.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a killed process group is given to exit on `SIGTERM` before it is
/// sent `SIGKILL`.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// The result of a supervised run.
#[derive(Debug)]
pub struct Supervised {
    /// The child's exit status and captured output.
    pub output: Output,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
    /// Whether poly killed the process for overrunning its budget. When set,
    /// `output.status` describes the *kill*, not the tool's own verdict.
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

/// Spawn `command` and wait for it under `budget`, killing it if it overruns.
///
/// `notify` is called with the elapsed time each time the announce cadence
/// comes due, on the supervising thread.
///
/// # Errors
///
/// Returns the spawn or wait error. The child (and its group) is killed before
/// a wait error propagates, so no supervised process outlives this call.
pub fn run(command: &mut Command, budget: Budget, notify: &dyn Fn(Duration)) -> std::io::Result<Supervised> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_process_group(command);

    let started = Instant::now();
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);

    let waited = wait_within(&mut child, budget, started, notify);
    if waited.is_err() {
        terminate(&mut child);
    }
    let (status, timed_out) = waited?;
    let elapsed = started.elapsed();

    Ok(Supervised {
        output: Output {
            status,
            stdout: join(stdout),
            stderr: join(stderr),
        },
        elapsed,
        timed_out,
    })
}

/// Wait for `child`, announcing and killing per `budget`. The `bool` is set
/// when the budget — not the child — ended the wait.
fn wait_within(
    child: &mut Child,
    budget: Budget,
    started: Instant,
    notify: &dyn Fn(Duration),
) -> std::io::Result<(ExitStatus, bool)> {
    let mut next_notice = budget.announce_after.map(|after| started + after);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        let now = Instant::now();
        if budget.limit.is_some_and(|limit| now.duration_since(started) >= limit) {
            terminate(child);
            return Ok((child.wait()?, true));
        }
        if let Some(due) = next_notice {
            if now >= due {
                notify(now.duration_since(started));
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

/// No-op on Windows: there is no process group to create here, and the kill
/// falls back to the direct child.
#[cfg(not(unix))]
fn isolate_process_group(_command: &mut Command) {}

/// Kill the child's process group: `SIGTERM`, then `SIGKILL` after a grace
/// period. Returns once nothing in the group is alive.
#[cfg(unix)]
fn terminate(child: &mut Child) {
    let Ok(group) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    // SAFETY: `kill` is a plain libc call with no memory operands. The negative
    // pid targets the process group whose leader is `group` — the group created
    // for this child by `isolate_process_group`, so nothing outside this hook's
    // own process tree can be signalled.
    unsafe { libc::kill(-group, libc::SIGTERM) };
    if exited_within(child, TERM_GRACE) {
        return;
    }
    // SAFETY: as above; the group could not be talked into exiting.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
}

/// Windows has no process groups here, so only the spawned process is killed.
#[cfg(not(unix))]
fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Poll for the child's exit for at most `grace`; `true` when it exited.
#[cfg(unix)]
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

/// Drain a pipe to EOF on its own thread, so a chatty child never blocks on a
/// full buffer while the supervisor is waiting on the clock.
fn drain<R: Read + Send + 'static>(mut reader: R) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

/// Collect a drain thread's bytes; a panicked reader yields what a dead pipe
/// would — nothing — rather than taking the run down with it.
fn join(handle: Option<JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle.and_then(|handle| handle.join().ok()).unwrap_or_default()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use super::run;
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

    #[test]
    fn a_child_that_finishes_in_time_reports_its_own_output_and_status() {
        let supervised = run(
            &mut sh("printf OUT; printf ERR >&2; exit 3"),
            budget(Some(Duration::from_secs(30))),
            &|_| {},
        )
        .expect("supervised run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.status.code(), Some(3));
        assert_eq!(supervised.output.stdout, b"OUT");
        assert_eq!(supervised.output.stderr, b"ERR");
    }

    #[test]
    fn a_child_that_overruns_is_killed_and_flagged() {
        let supervised = run(&mut sh("sleep 30"), budget(Some(Duration::from_millis(150))), &|_| {}).expect("run");

        assert!(supervised.timed_out, "the overrun must be reported as such");
        assert!(supervised.elapsed >= Duration::from_millis(150));
        assert!(
            supervised.elapsed < Duration::from_secs(30),
            "the supervisor must not have waited for the child"
        );
    }

    #[test]
    fn a_still_running_child_announces_itself_before_it_is_killed() {
        use std::sync::Mutex;

        let seen: Mutex<Vec<Duration>> = Mutex::new(Vec::new());
        let announce = Budget {
            limit: Some(Duration::from_millis(400)),
            announce_after: Some(Duration::from_millis(50)),
            announce_every: Duration::from_millis(50),
        };
        let supervised = run(&mut sh("sleep 30"), announce, &|elapsed| {
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

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let supervised = run(
            &mut sh(
                "i=0; while [ $i -lt 4000 ]; do printf '0123456789012345678901234567890123456789\\n'; \
                     i=$((i+1)); done",
            ),
            budget(Some(Duration::from_secs(30))),
            &|_| {},
        )
        .expect("run");

        assert!(!supervised.timed_out);
        assert_eq!(supervised.output.stdout.len(), 4000 * 41);
    }
}
