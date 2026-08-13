//! The lines poly prints *while* hooks are still in flight — the liveness
//! vocabulary.
//!
//! Separate from the final report because it answers a different question and
//! changes for a different reason. The report in [`super`] explains a verdict
//! after the fact; these lines exist so a silent terminal names its culprit as
//! it happens, and they go to stderr (or above the live spinners) — never into
//! the rendered report. Every kind of quiet poly can tell apart gets its own
//! line and its own marker here, and that set grows whenever poly learns to
//! recognise another kind of waiting.

use std::time::Duration;

use super::format_duration;
use super::markers::{LOCK_WAIT_MARKER, RUNNING_MARKER};

/// The line a hook prints while it is still running, so a hang names its
/// culprit as it happens instead of leaving a silent terminal.
///
/// Written to stderr (or above the live spinners), never to the final report.
/// Naming the budget alongside the elapsed time answers the reader's actual
/// question — "is this thing ever going to stop?" — rather than only "it is
/// slow".
///
/// `waiting_on` is the resource the hook last reported being queued behind (see
/// [`crate::supervise::LockWait`]). A hook blocked on cargo's package-cache or
/// build-directory lock, held by a cargo process poly did not start, produces no
/// output at all — so "still running" reads as *working* when the truth is
/// *waiting its turn*. The two get different lines and different markers, and
/// the notice says outright that the budget is still being charged, since poly
/// does not pause it for a queue it cannot see.
#[must_use]
pub fn still_running_line(id: &str, elapsed: Duration, limit: Option<Duration>, waiting_on: Option<&str>) -> String {
    let elapsed = format_duration(elapsed);
    let deadline = match limit {
        Some(limit) => format!("killed at {}", format_duration(limit)),
        None => "no timeout".to_string(),
    };
    match waiting_on {
        Some(resource) => format!(
            "  {LOCK_WAIT_MARKER} waiting on a lock: {id} ({elapsed} elapsed, {deadline}) — \
             blocked on cargo's {resource} lock held by a process outside this run, doing no work; \
             the time budget is still counting"
        ),
        None => format!("  {RUNNING_MARKER} still running: {id} ({elapsed} elapsed, {deadline})"),
    }
}

/// The line a hook prints while poly is holding it back, **before** it is
/// spawned, because a process outside this run holds the lock it would block on
/// (see [`crate::cargo_lock`]).
///
/// It mirrors [`still_running_line`]'s queued form — same marker, same "waiting"
/// vocabulary, the same resource name — so the two lock waits read as one
/// concept. What it must not share is the closing clause: this hook has not been
/// spawned, so its budget has *not* started, and saying otherwise would describe
/// the exact defect this wait exists to remove. It names the moment poly will
/// give up and start the hook anyway, so the wait is bounded on screen and not
/// only in the code.
#[must_use]
pub fn lock_wait_line(id: &str, waited: Duration, bound: Duration, resource: &str) -> String {
    let waited = format_duration(waited);
    let bound = format_duration(bound);
    format!(
        "  {LOCK_WAIT_MARKER} waiting to start: {id} ({waited} waited, starting anyway at {bound}) — \
         cargo's {resource} lock is held by a process outside this run; \
         the hook has not been spawned and its time budget has not started"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::markers::TIMED_OUT_MARKER;

    #[test]
    fn still_running_line_names_the_hook_and_when_it_will_be_killed() {
        assert_eq!(
            still_running_line("clippy", Duration::from_secs(15), Some(Duration::from_mins(30)), None),
            "  ⋯ still running: clippy (15.0s elapsed, killed at 1800.0s)"
        );
        assert_eq!(
            still_running_line("clippy", Duration::from_millis(750), None, None),
            "  ⋯ still running: clippy (750ms elapsed, no timeout)"
        );
    }

    /// A queued hook and a wedged hook were the same line. The queued one now
    /// says what it is waiting for, that it is doing no work, and — since poly
    /// does not pause the budget for a queue it cannot see — that the clock is
    /// still running.
    #[test]
    fn a_hook_queued_on_a_lock_says_so_instead_of_claiming_it_is_running() {
        assert_eq!(
            still_running_line(
                "cargo:cargo-deny",
                Duration::from_secs(45),
                Some(Duration::from_mins(30)),
                Some("package cache"),
            ),
            "  ⏸ waiting on a lock: cargo:cargo-deny (45.0s elapsed, killed at 1800.0s) — blocked on cargo's \
             package cache lock held by a process outside this run, doing no work; the time budget is still counting"
        );
        assert_eq!(
            still_running_line("cargo:clippy", Duration::from_secs(90), None, Some("build directory")),
            "  ⏸ waiting on a lock: cargo:clippy (90.0s elapsed, no timeout) — blocked on cargo's \
             build directory lock held by a process outside this run, doing no work; the time budget is still counting"
        );
    }

    /// The pre-spawn wait is a fourth state, and it says the opposite of the
    /// post-spawn one about the budget: the hook has not been spawned, so its
    /// clock has not started. Reading these two lines the same way is the
    /// misreport the wait exists to prevent.
    #[test]
    fn a_hook_held_back_before_it_is_spawned_says_its_budget_has_not_started() {
        assert_eq!(
            lock_wait_line(
                "cargo:cargo-deny",
                Duration::from_secs(4),
                Duration::from_mins(15),
                "package cache",
            ),
            "  ⏸ waiting to start: cargo:cargo-deny (4.0s waited, starting anyway at 900.0s) — cargo's \
             package cache lock is held by a process outside this run; the hook has not been spawned and \
             its time budget has not started"
        );
    }

    /// Four states, four readings: queued before spawn, running, queued after
    /// spawn, killed. No line may be mistaken for another.
    #[test]
    fn the_four_hook_states_are_never_mistakable_for_one_another() {
        let elapsed = Duration::from_secs(45);
        let limit = Duration::from_mins(30);
        let before = lock_wait_line("cargo-deny", elapsed, limit / 2, "package cache");
        let after = still_running_line("cargo-deny", elapsed, Some(limit), Some("package cache"));
        let running = still_running_line("cargo-deny", elapsed, Some(limit), None);

        assert!(
            before.contains("its time budget has not started"),
            "a hook that was never spawned has no clock running: {before}"
        );
        assert!(
            after.contains("the time budget is still counting"),
            "a spawned hook's clock keeps running whatever it is blocked on: {after}"
        );
        assert!(
            !before.contains("still running") && !running.contains("waiting"),
            "a held-back hook and a working hook must not read alike"
        );
        assert_ne!(before, after, "the two lock waits are different states");

        // The fourth state is a final one and carries its own glyph; a wait must
        // never be able to render as a kill.
        assert_eq!(TIMED_OUT_MARKER, "⧖");
        assert!(
            !before.contains(TIMED_OUT_MARKER) && !after.contains(TIMED_OUT_MARKER),
            "a hook that is waiting has not been killed"
        );
    }

    #[test]
    fn the_lock_wait_notice_is_never_mistakable_for_the_still_running_notice() {
        let elapsed = Duration::from_secs(45);
        let limit = Some(Duration::from_mins(30));
        let waiting = still_running_line("cargo-deny", elapsed, limit, Some("package cache"));
        let running = still_running_line("cargo-deny", elapsed, limit, None);

        assert_ne!(LOCK_WAIT_MARKER, RUNNING_MARKER);
        assert_eq!(LOCK_WAIT_MARKER, "⏸");
        assert!(
            !waiting.contains("still running"),
            "a queued hook must not claim to be running: {waiting}"
        );
        assert!(
            !running.contains("waiting on a lock"),
            "a working hook must not claim to be queued: {running}"
        );
    }
}
