//! Running a built command to completion — or to the end of its budget — and
//! turning what happened into a [`HookStatus`].
//!
//! Separate from command *construction* because this half owns the run's
//! liveness concerns rather than its argv: supervision and the kill deadline,
//! the still-running notice, the cargo package-cache wait, and the
//! classification rules that keep a killed process from being reported as a
//! plain failure. Those rules are what stand between a wedged check and a
//! false pass, so they are worth reading without a shell quoting table in the
//! way.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use indicatif::ProgressBar;
use tracing::warn;

use super::shell::{SHELL, SHELL_ARG};
use crate::cargo_lock::{self, PACKAGE_CACHE_RESOURCE, Wait, WaitPlan};
use crate::model::{HookStatus, StepOutcome, TimeoutReason};
use crate::process::{Cmd, Error as ProcessError, OutputSink};
use crate::reporter::{CaptureSink, PreviewSink};
use crate::supervise::Supervised;
use crate::timeout::Budget;

/// Run one command to completion — or to the end of its `budget` — capturing
/// its combined output.
///
/// When `bar` is present the output is streamed live into the hook's spinner
/// via a [`PreviewSink`]; otherwise a plain [`CaptureSink`] just accumulates
/// it. While the command runs, the budget's announce cadence prints a
/// still-running notice naming `id`, so a hang is attributable while it is
/// happening rather than only in hindsight.
pub(crate) fn execute(mut cmd: Cmd, bar: Option<&ProgressBar>, id: &str, budget: Budget) -> (HookStatus, Vec<u8>) {
    cmd.check(false);
    let notify = |elapsed: Duration, waiting_on: Option<&str>| {
        announce_still_running(bar, id, elapsed, budget.limit, waiting_on);
    };
    let (result, bytes) = if let Some(bar) = bar {
        let mut sink = PreviewSink::new(bar, id);
        let result = capture(&mut cmd, &mut sink, budget, &notify);
        (result, sink.into_bytes())
    } else {
        let mut sink = CaptureSink::default();
        let result = capture(&mut cmd, &mut sink, budget, &notify);
        (result, sink.into_bytes())
    };
    match result {
        Ok(run) => (status_of(&run, budget), bytes),
        Err(error) => (HookStatus::Error(error.to_string()), bytes),
    }
}

/// Run `cmd` under `budget`, falling back to the plain unbounded capture when
/// the budget neither kills nor announces — so disabling timeouts restores the
/// previous execution path exactly, threads and all.
fn capture<S: OutputSink>(
    cmd: &mut Cmd,
    sink: S,
    budget: Budget,
    notify: &dyn Fn(Duration, Option<&str>),
) -> Result<Supervised, ProcessError> {
    if budget.is_supervised() {
        return cmd.output_with_sink_supervised(sink, budget, notify);
    }
    let started = Instant::now();
    cmd.output_with_sink(sink)
        .map(|output| Supervised::completed(output, started.elapsed()))
}

/// Classify a completed run. A killed process is reported as
/// [`HookStatus::TimedOut`] and never as a plain failure: its exit status
/// describes poly's signal, not the tool's opinion of the code.
///
/// Reading the kill flag ahead of the status is only sound because
/// [`Supervised::timed_out`] means *the kill is what ended the child*, not
/// merely that poly went to kill it — a child that finished on its own at the
/// deadline arrives here with the flag clear and is classified on its own exit
/// code, which is the difference between reporting a passing hook and blocking
/// a commit on it.
fn status_of(run: &Supervised, budget: Budget) -> HookStatus {
    if run.timed_out {
        return HookStatus::TimedOut(TimeoutReason::command(budget.limit.unwrap_or(run.elapsed), run.elapsed));
    }
    if run.output.status.success() {
        HookStatus::Passed
    } else {
        HookStatus::Failed {
            code: run.output.status.code(),
        }
    }
}

/// Put the running hook's id on screen while it is still running.
///
/// Routed through the progress bar when there is one (so it lands above the
/// live spinners instead of tearing them), and straight to stderr otherwise —
/// which is the case that matters, because a non-interactive run has no spinner
/// to reveal what is hanging.
///
/// `waiting_on` names the lock the hook is queued behind, when it said so:
/// "quiet because it is working" and "quiet because cargo will not let it start"
/// are different diagnoses and the notice reports whichever one is true.
fn announce_still_running(
    bar: Option<&ProgressBar>,
    id: &str,
    elapsed: Duration,
    limit: Option<Duration>,
    waiting_on: Option<&str>,
) {
    announce(bar, crate::reporter::still_running_line(id, elapsed, limit, waiting_on));
}

/// Put `line` on screen above the live spinners, or on stderr when nothing is
/// drawing them.
///
/// A hidden bar swallows `println`, which is precisely the case that must not
/// stay silent: progress requested, but nothing is drawing it.
fn announce(bar: Option<&ProgressBar>, line: String) {
    match bar.filter(|bar| !bar.is_hidden()) {
        Some(bar) => bar.println(line),
        None => eprintln!("{line}"),
    }
}

/// Hold `id` back until cargo's package-cache lock is free, so a holder outside
/// this run is not charged against the hook's budget.
///
/// Called only for a hook in the cargo exclusion set ([`crate::model::Hook::is_cargo`]),
/// and only from the runner, which knows what the hook is; the supervisor sees an
/// argv it cannot classify and must not try.
///
/// This is a mitigation and not a fix — the lock can be taken between the probe
/// and the spawn, and cargo re-takes it mid-run — so the hook is spawned when the
/// bound expires rather than being withheld: a check that did not run must never
/// be reported as a pass, and running late beats failing a commit for somebody
/// else's `cargo build`. From that point the post-spawn notice, driven by the
/// child's own `Blocking waiting for file lock` line, describes the rest.
pub(crate) fn await_cargo_package_cache(id: &str, budget: Budget, bar: Option<&ProgressBar>) {
    let (Some(path), Some(plan)) = (cargo_lock::package_cache_lock(), WaitPlan::for_budget(budget)) else {
        return;
    };
    let waited = cargo_lock::wait_until_free(&path, plan, &|waited| {
        announce(
            bar,
            crate::reporter::lock_wait_line(id, waited, plan.bound, PACKAGE_CACHE_RESOURCE),
        );
    });
    if let Wait::Expired(waited) = waited {
        warn!(
            hook = id,
            "cargo's {PACKAGE_CACHE_RESOURCE} lock was still held after {waited:.1?}; starting the hook anyway — \
             its budget now includes whatever is left of that wait"
        );
    }
}

/// Run a `before`/`after` shell command from `root` under `budget`, capturing
/// its output.
///
/// `env` is layered over the inherited environment — empty for a stage-level
/// step, the hook's own declared `env` for a per-hook one, so a hook's setup
/// sees exactly what the hook will.
///
/// A step that overruns is killed exactly as a hook is, and reports
/// [`HookStatus::TimedOut`]: a setup step that hangs blocks the commit just as
/// completely as a hook that hangs, and the caller has to be able to tell that
/// apart from a step that failed on its own merits.
pub(crate) fn run_step(root: &Path, command: &str, env: &BTreeMap<String, String>, budget: Budget) -> StepOutcome {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root).envs(env.iter());
    let (status, output) = execute(cmd, None, command, budget);
    StepOutcome {
        command: command.to_string(),
        status,
        output,
    }
}

/// What a `precondition` probe answered.
///
/// [`Probe::TimedOut`] exists because the alternative is a false pass: reading
/// a wedged probe as "does not apply" would withhold every hook it guards and
/// let the run report success having validated nothing.
pub(crate) enum Probe {
    /// Exit 0 — the guarded hooks apply.
    Passed,
    /// Non-zero, or the probe could not be launched — they do not apply.
    Declined,
    /// poly killed the probe; whether they apply is unknown.
    TimedOut(TimeoutReason),
}

/// Evaluate a `precondition` guard from `root` under `budget`.
///
/// Output is discarded — a precondition is a probe, not a check, so its chatter
/// never reaches the report.
pub(crate) fn run_precondition(root: &Path, command: &str, env: &BTreeMap<String, String>, budget: Budget) -> Probe {
    let mut cmd = Cmd::new(SHELL, command.to_string());
    cmd.arg(SHELL_ARG).arg(command).current_dir(root).envs(env.iter());

    if !budget.is_supervised() {
        // The un-supervised path is kept byte for byte: no pipes, no drain
        // threads, output straight to /dev/null.
        cmd.stdout(Stdio::null()).stderr(Stdio::null()).check(false);
        return match cmd.status() {
            Ok(status) if status.success() => Probe::Passed,
            _ => Probe::Declined,
        };
    }

    // Supervision needs pipes, so the output is captured and dropped rather
    // than never produced — the same nothing, from the report's point of view.
    match execute(cmd, None, command, budget).0 {
        HookStatus::Passed => Probe::Passed,
        HookStatus::TimedOut(reason) => Probe::TimedOut(TimeoutReason::precondition(reason.limit, reason.elapsed)),
        _ => Probe::Declined,
    }
}
