//! Result rendering helpers for hook execution output.
//!
//! Ported from `polyhooks/src/cli/run/reporter.rs`. Two families live here:
//!
//! - **Final render** (this module) — [`HookRunReporter`] turns a completed
//!   [`HookRunOutcome`](crate::model::HookRunOutcome) into a deterministic,
//!   non-interleaved report, with the standalone helpers
//!   [`project_status_marker`] and [`truncate_to_width`], plus the live
//!   [`still_running_line`] notice.
//! - **Live progress** ([`progress`]) — the spinner UI, the rolling
//!   [`OutputPreview`] window, and the output sinks that feed them. Re-exported
//!   here, so the module split is invisible to callers.
//!
//! The report's job is to keep distinct states distinct: a hook that was
//! skipped, one poly killed, one whose setup failed, and one whose fix could not
//! be delivered all get their own marker and their own sentence, because each
//! asks the reader for something different.

use std::borrow::Cow;
use std::time::Duration;

use console::strip_ansi_codes;
use owo_colors::OwoColorize as _;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

pub mod progress;

pub use progress::{
    CaptureSink, HOOK_OUTPUT_PREVIEW_LINES, HOOK_OUTPUT_PREVIEW_PREFIX, HookBar, OutputPreview, PreviewSink, ProgressUi,
};

/// Return a coloured pass/fail status marker: "✓" (green) or "×" (red).
#[must_use]
pub fn project_status_marker(failed: bool) -> String {
    if failed {
        "×".red().to_string()
    } else {
        "✓".green().to_string()
    }
}

/// Truncate `input` so its Unicode display width fits within `width` columns.
///
/// When truncation is needed, the last three characters are replaced with
/// `"..."`. Returns the original string borrowed when no truncation is needed.
pub fn truncate_to_width(input: &str, width: usize) -> Cow<'_, str> {
    if input.width() <= width {
        return Cow::Borrowed(input);
    }

    if width <= 3 {
        return Cow::Owned(".".repeat(width));
    }

    let mut output = String::new();
    let mut used = 0usize;
    let target = width - 3;
    for ch in input.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > target {
            break;
        }
        output.push(ch);
        used += ch_width;
    }
    output.push_str("...");
    Cow::Owned(output)
}

/// At or above this many seconds a duration renders as `s` (e.g. `1.2s`); below
/// it, as whole milliseconds (e.g. `340ms`).
const SECS_DISPLAY_THRESHOLD: f64 = 1.0;

/// Format a hook duration compactly: `1.2s` at or above a second, else `340ms`.
pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= SECS_DISPLAY_THRESHOLD {
        format!("{secs:.1}s")
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// Renders a completed [`HookRunOutcome`](crate::model::HookRunOutcome) into a
/// deterministic, non-interleaved report.
///
/// All hooks are reported in position order (the runner sorts them), and each
/// hook's captured output is emitted as one contiguous block — there is no live
/// progress UI and no chunk interleaving, so the output is reproducible.
#[derive(Debug, Default)]
pub struct HookRunReporter;

impl HookRunReporter {
    /// Create a reporter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Render the whole run to a `String`.
    #[allow(clippy::unused_self)]
    #[must_use]
    pub fn render(&self, outcome: &crate::model::HookRunOutcome) -> String {
        let mut report = String::new();
        for stage in &outcome.stages {
            Self::render_stage(&mut report, stage);
        }
        Self::render_nothing_validated(&mut report, outcome);
        Self::render_legend(&mut report, outcome);
        report
    }

    /// Explain every non-verdict marker the report actually used.
    ///
    /// `-`, `?` and `⧖` each mean something different and none of them is
    /// guessable, so the report says so — but only when one of them appears,
    /// since a run where every hook passed needs no key.
    fn render_legend(report: &mut String, outcome: &crate::model::HookRunOutcome) {
        use std::fmt::Write as _;

        use crate::model::HookStatus;

        let statuses = || outcome.stages.iter().flat_map(|stage| stage.hooks.iter());
        // Steps carry the same markers as hooks, so a run whose only casualty
        // was a `before` step still needs the key that explains `⧖`.
        let step_statuses = || {
            outcome.stages.iter().flat_map(|stage| {
                stage
                    .before
                    .iter()
                    .chain(stage.after.iter())
                    .chain(stage.hooks.iter().flat_map(|hook| hook.before.iter()))
                    .map(|step| &step.status)
            })
        };
        let mut entries: Vec<String> = Vec::new();
        if statuses().any(|hook| matches!(hook.status, HookStatus::Skipped(_))) {
            entries.push(format!("{SKIPPED_MARKER} skipped (did not apply)"));
        }
        if statuses().any(|hook| matches!(hook.status, HookStatus::TimedOut(_)))
            || step_statuses().any(|status| matches!(status, HookStatus::TimedOut(_)))
        {
            entries.push(format!("{} killed by poly on timeout", timed_out_marker()));
        }
        if statuses().any(|hook| matches!(hook.status, HookStatus::Unknown(_))) {
            entries.push(format!("{} not run (setup failed)", unknown_marker()));
        }
        if entries.is_empty() {
            return;
        }
        let _ = writeln!(
            report,
            "  markers: {} passed  {} failed  {}",
            project_status_marker(false),
            project_status_marker(true),
            entries.join("  ")
        );
    }

    /// Spell out, in the report a human reads, that the run checked nothing —
    /// the state that used to be indistinguishable from a clean pass.
    fn render_nothing_validated(report: &mut String, outcome: &crate::model::HookRunOutcome) {
        use std::fmt::Write as _;

        if !outcome.validated_nothing() {
            return;
        }
        let withheld = outcome.precondition_skipped_count();
        let _ = writeln!(
            report,
            "  {NOTHING_VALIDATED_MARKER} nothing was validated: \
             {withheld} configured hook(s) were withheld by a precondition and none ran"
        );
    }

    fn render_stage(report: &mut String, stage: &crate::model::StageOutcome) {
        use std::fmt::Write as _;

        use crate::model::StageStatus;

        // A stage that ran but bound no work emits nothing: with all ten shims
        // installed, unconfigured stages fire on ordinary git operations, and an
        // empty `[stage] <name>` banner is noise. A skipped/aborted stage still
        // renders its reason below.
        if matches!(stage.status, StageStatus::Ran)
            && stage.before.is_empty()
            && stage.hooks.is_empty()
            && stage.after.is_empty()
        {
            return;
        }

        let _ = writeln!(report, "[stage] {}{}", stage.stage, validated_banner(stage));
        match &stage.status {
            StageStatus::Skipped(reason) => {
                let _ = writeln!(report, "  skipped: {reason}");
            }
            StageStatus::Aborted(reason) => {
                let _ = writeln!(report, "  aborted: {reason}");
            }
            StageStatus::Ran => {}
        }

        for step in &stage.before {
            Self::render_step(report, "before", step);
        }

        for hook in &stage.hooks {
            Self::render_hook(report, hook);
        }

        for step in &stage.after {
            Self::render_step(report, "after", step);
        }
    }

    /// Render one stage-level `before`/`after` step.
    ///
    /// A killed step gets the timeout marker and says so: "poly stopped this
    /// setup command" and "this setup command said no" are different facts, and
    /// a shared `×` would put the reader back to guessing.
    fn render_step(report: &mut String, label: &str, step: &crate::model::StepOutcome) {
        use std::fmt::Write as _;

        let _ = writeln!(
            report,
            "  {} {label}: {}{}",
            step_marker(&step.status),
            step.command,
            step_note(&step.status)
        );
        append_failure_output(report, &step.status, &step.output);
    }

    /// Render one hook line. Every hook the runner knows about is rendered —
    /// including ones that never executed — because a check that silently
    /// vanishes from the report is how "nothing ran" stays invisible.
    fn render_hook(report: &mut String, hook: &crate::model::HookOutcome) {
        use std::fmt::Write as _;

        use crate::model::HookStatus;

        let (marker, note) = match &hook.status {
            HookStatus::Skipped(reason) => (SKIPPED_MARKER.to_string(), format!(" ({reason})")),
            HookStatus::Unknown(reason) => (unknown_marker(), format!(" (not run — {reason})")),
            HookStatus::TimedOut(reason) => (timed_out_marker(), format!(" ({reason})")),
            status => (project_status_marker(status.is_failure()), String::new()),
        };
        let suffix = if hook.files_modified {
            " (files modified)"
        } else if hook.cached {
            " (cached)"
        } else {
            ""
        };
        let _ = writeln!(report, "  {marker} {}{suffix}{note}", hook.id);
        for step in &hook.before {
            if step.status.is_failure() {
                let _ = writeln!(report, "      before: {}{}", step.command, step_note(&step.status));
                append_failure_output(report, &step.status, &step.output);
            }
        }
        if let HookStatus::FixWithheld(withheld) = &hook.status {
            append_withheld_fix(report, withheld);
        }
        append_failure_output(report, &hook.status, &hook.output);
    }
}

/// The " — validated <tree>" suffix on a stage banner.
///
/// A gate that does not say which bytes it read cannot be trusted to have read
/// the right ones, so the tree is always named. When hooks disagree — which the
/// runner does not currently produce, but which a future scoping exception
/// could — the banner says so instead of picking one, and
/// [`HookRunReporter::render_hook`] is where the per-hook detail would go.
fn validated_banner(stage: &crate::model::StageOutcome) -> String {
    let mut trees = stage.hooks.iter().map(|hook| hook.validated);
    let Some(first) = trees.next() else {
        return String::new();
    };
    if trees.all(|tree| tree == first) {
        format!(" — validated {first}")
    } else {
        " — validated MIXED trees (see each hook)".to_string()
    }
}

/// Spell out a withheld fix: which files, why each one was withheld, and what
/// the author has to do about it. The hook exited 0, so without this the report
/// would show a failure with no output at all.
///
/// Each path carries its own reason. Rendering them all as "unstaged changes"
/// — as this used to — is wrong for every case but one, and for a security
/// refusal it is worse than wrong: it sends the author to `git add` a symlink
/// poly will never write through, when what the tree actually contains is a
/// path that escapes the repository.
fn append_withheld_fix(report: &mut String, withheld: &[crate::model::WithheldFix]) {
    use std::fmt::Write as _;

    use crate::model::WithheldReason;

    let _ = writeln!(report, "      fixed the staged content, but the fix was not written:");
    for fix in withheld {
        let marker = if fix.reason.is_security_refusal() {
            format!("{SECURITY_REFUSAL_LABEL} ")
        } else {
            String::new()
        };
        let _ = writeln!(report, "        {marker}{} — {}", fix.path.display(), fix.reason);
    }
    let reasons = |predicate: fn(&WithheldReason) -> bool| withheld.iter().any(|fix| predicate(&fix.reason));
    if reasons(|reason| matches!(reason, WithheldReason::UnstagedChanges)) {
        let _ = writeln!(
            report,
            "      re-run the fixer over your working tree and `git add` the result, \
             or stash the unstaged changes first."
        );
    }
    if reasons(WithheldReason::is_security_refusal) {
        let _ = writeln!(
            report,
            "      poly will not write a fix through a symlink or to a path outside the repository. \
             Staging changes nothing here — replace the tracked entry with a regular file inside \
             the repository, and check where the existing one points."
        );
    }
    if reasons(|reason| {
        matches!(
            reason,
            WithheldReason::WorktreeNotRegularFile | WithheldReason::SnapshotUnreadable
        )
    }) {
        let _ = writeln!(
            report,
            "      there was no readable file to write, so nothing was changed — \
             check the path still exists as a regular file."
        );
    }
}

/// Marker for a hook a precondition (or an empty file set) withheld — benign.
const SKIPPED_MARKER: &str = "-";

/// Label leading a withheld-fix line poly declined for **safety**, not for the
/// author's convenience: a symlink destination or a path leaving the repository.
///
/// Shouted, and never applied to the unstaged-changes case, because the two ask
/// the reader for entirely different things and only one of them is a finding
/// about the repository's contents.
const SECURITY_REFUSAL_LABEL: &str = "SECURITY:";

/// Marker for the "nothing was validated" summary line.
const NOTHING_VALIDATED_MARKER: &str = "!";

/// Marker for a hook still executing, used by the live still-running notice.
/// Never a final status — its whole job is to be distinguishable from
/// [`SKIPPED_MARKER`], because "has not finished" and "did not apply" were the
/// two facts a single `-` used to blur together.
const RUNNING_MARKER: &str = "⋯";

/// Marker for a hook poly killed for overrunning its budget.
const TIMED_OUT_MARKER: &str = "⧖";

/// Marker for a hook that is quiet because it is **queued** behind a lock poly
/// does not own, not because it is working.
///
/// Distinct from [`RUNNING_MARKER`] on purpose: a hook waiting on cargo's
/// package-cache lock and a hook grinding through a build are both silent, and a
/// shared glyph would leave the reader to guess which one they are watching.
const LOCK_WAIT_MARKER: &str = "⏸";

/// Marker for a hook whose verdict is unknown because its setup failed. Distinct
/// from both `✓/×` (a real verdict) and `-` (a benign skip).
fn unknown_marker() -> String {
    "?".yellow().to_string()
}

/// Marker for a hook poly killed — a failure, but not the tool's own.
fn timed_out_marker() -> String {
    TIMED_OUT_MARKER.red().to_string()
}

/// The marker a `before`/`after` step line carries: its own verdict, unless
/// poly killed it.
fn step_marker(status: &crate::model::HookStatus) -> String {
    match status {
        crate::model::HookStatus::TimedOut(_) => timed_out_marker(),
        other => project_status_marker(other.is_failure()),
    }
}

/// The parenthetical a step line carries. Only a kill needs one — every other
/// step status is fully described by its marker and its captured output.
fn step_note(status: &crate::model::HookStatus) -> String {
    match status {
        crate::model::HookStatus::TimedOut(reason) => format!(" ({reason})"),
        _ => String::new(),
    }
}

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

fn append_failure_output(report: &mut String, status: &crate::model::HookStatus, output: &[u8]) {
    use std::fmt::Write as _;

    if !status.is_failure() {
        return;
    }
    let text = String::from_utf8_lossy(output);
    let text = strip_ansi_codes(&text);
    for line in text.lines() {
        let _ = writeln!(report, "      {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_marker_pass() {
        let s = project_status_marker(false);
        assert!(s.contains('✓'));
    }

    #[test]
    fn status_marker_fail() {
        let s = project_status_marker(true);
        assert!(s.contains('×'));
    }

    #[test]
    fn truncate_no_op_when_short_enough() {
        let s = "hello";
        assert!(matches!(truncate_to_width(s, 10), Cow::Borrowed(_)));
    }

    #[test]
    fn truncate_adds_ellipsis() {
        let result = truncate_to_width("abcdefghijklmno", 10);
        assert!(result.ends_with("..."));
        assert!(result.width() <= 10);
    }

    #[test]
    fn truncate_very_narrow_target() {
        assert_eq!(truncate_to_width("hello", 2), "..");
        assert_eq!(truncate_to_width("hello", 0), "");
    }

    /// A finished hook outcome carrying `status`, for rendering assertions.
    fn outcome_with(id: &str, status: crate::model::HookStatus) -> crate::model::HookOutcome {
        crate::model::HookOutcome {
            id: id.to_string(),
            position: 0,
            status,
            before: Vec::new(),
            files_modified: false,
            output: Vec::new(),
            duration: Duration::from_secs(1),
            cached: false,
            validated: crate::model::ValidatedTree::Worktree,
        }
    }

    #[test]
    fn timed_out_hook_renders_its_own_marker_and_says_poly_killed_it() {
        use crate::model::{HookStatus, TimeoutReason};

        let hook = outcome_with(
            "ai-rulez:ai-rulez-validate",
            HookStatus::TimedOut(TimeoutReason::command(
                Duration::from_mins(10),
                Duration::from_secs_f64(600.4),
            )),
        );
        let mut report = String::new();
        HookRunReporter::render_hook(&mut report, &hook);
        assert_eq!(
            report,
            format!(
                "  {} ai-rulez:ai-rulez-validate (timed out: poly killed it after 600.4s, limit 600.0s)\n",
                timed_out_marker()
            )
        );
    }

    #[test]
    fn skipped_and_timed_out_markers_are_not_the_same_glyph() {
        assert_eq!(SKIPPED_MARKER, "-");
        assert_eq!(TIMED_OUT_MARKER, "⧖");
        assert_eq!(RUNNING_MARKER, "⋯");
        assert_ne!(SKIPPED_MARKER, TIMED_OUT_MARKER);
        assert_ne!(SKIPPED_MARKER, RUNNING_MARKER);
    }

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

    /// A withheld fix used to blame unstaged changes whatever the cause. Each
    /// path now carries its own reason, and only the reasons present get their
    /// remedy.
    #[test]
    fn a_withheld_fix_reports_each_path_with_its_own_reason() {
        use crate::model::{HookStatus, WithheldFix, WithheldReason};

        let hook = outcome_with(
            "fmt",
            HookStatus::FixWithheld(vec![
                WithheldFix::new("src/lib.rs", WithheldReason::UnstagedChanges),
                WithheldFix::new("evil.rs", WithheldReason::WorktreeIsSymlink),
            ]),
        );
        let mut report = String::new();
        HookRunReporter::render_hook(&mut report, &hook);

        let expected = [
            format!("  {} fmt\n", project_status_marker(true)),
            "      fixed the staged content, but the fix was not written:\n".to_string(),
            "        src/lib.rs — the worktree copy has unstaged changes the fix never saw\n".to_string(),
            "        SECURITY: evil.rs — the worktree entry is a symlink; \
             poly refused to write through it\n"
                .to_string(),
            "      re-run the fixer over your working tree and `git add` the result, \
             or stash the unstaged changes first.\n"
                .to_string(),
            "      poly will not write a fix through a symlink or to a path outside the repository. \
             Staging changes nothing here — replace the tracked entry with a regular file inside \
             the repository, and check where the existing one points.\n"
                .to_string(),
        ]
        .concat();
        assert_eq!(report, expected);
    }

    /// THE MISLEADING MESSAGE. A path that escapes the repository is a security
    /// refusal; telling that author to stage their work sends them to fix
    /// something that is not broken and hides what is.
    #[test]
    fn a_security_refusal_never_blames_unstaged_changes() {
        use crate::model::{HookStatus, WithheldFix, WithheldReason};

        let hook = outcome_with(
            "fmt",
            HookStatus::FixWithheld(vec![WithheldFix::new(
                "../../etc/shadow",
                WithheldReason::PathEscapesRepository,
            )]),
        );
        let mut report = String::new();
        HookRunReporter::render_hook(&mut report, &hook);

        assert!(
            report.contains(
                "        SECURITY: ../../etc/shadow — the path leaves the repository; \
                 poly refused to write outside it\n"
            ),
            "the refusal must name the file and say poly refused it: {report}"
        );
        assert!(
            !report.contains("unstaged"),
            "a security refusal has nothing to do with unstaged work: {report}"
        );
        assert!(
            !report.contains("git add"),
            "staging cannot make this fix land, so the report must not suggest it: {report}"
        );
    }

    #[test]
    fn an_unreadable_path_is_neither_a_security_refusal_nor_unstaged_work() {
        use crate::model::{HookStatus, WithheldFix, WithheldReason};

        let hook = outcome_with(
            "fmt",
            HookStatus::FixWithheld(vec![
                WithheldFix::new("gone.rs", WithheldReason::WorktreeNotRegularFile),
                WithheldFix::new("snap.rs", WithheldReason::SnapshotUnreadable),
            ]),
        );
        let mut report = String::new();
        HookRunReporter::render_hook(&mut report, &hook);

        let expected = [
            format!("  {} fmt\n", project_status_marker(true)),
            "      fixed the staged content, but the fix was not written:\n".to_string(),
            "        gone.rs — the worktree entry is not a regular file\n".to_string(),
            "        snap.rs — the fixed copy in poly's staged snapshot could not be read\n".to_string(),
            "      there was no readable file to write, so nothing was changed — \
             check the path still exists as a regular file.\n"
                .to_string(),
        ]
        .concat();
        assert_eq!(report, expected);
        assert!(!report.contains(SECURITY_REFUSAL_LABEL), "not a refusal: {report}");
    }

    #[test]
    fn legend_explains_only_the_markers_the_report_used() {
        use crate::model::{HookRunOutcome, HookStatus, SkipReason, StageOutcome, StageStatus, TimeoutReason};
        use crate::stage::Stage;

        let outcome = HookRunOutcome {
            stages: vec![StageOutcome {
                stage: Stage::PreCommit,
                status: StageStatus::Ran,
                before: Vec::new(),
                hooks: vec![
                    outcome_with("a", HookStatus::Skipped(SkipReason::NoFiles)),
                    outcome_with(
                        "b",
                        HookStatus::TimedOut(TimeoutReason::command(
                            Duration::from_mins(10),
                            Duration::from_secs(601),
                        )),
                    ),
                ],
                after: Vec::new(),
            }],
        };
        let mut report = String::new();
        HookRunReporter::render_legend(&mut report, &outcome);
        assert_eq!(
            report,
            format!(
                "  markers: {} passed  {} failed  - skipped (did not apply)  {} killed by poly on timeout\n",
                project_status_marker(false),
                project_status_marker(true),
                timed_out_marker()
            )
        );
    }

    #[test]
    fn legend_is_omitted_when_every_hook_reported_a_verdict() {
        use crate::model::{HookRunOutcome, HookStatus, StageOutcome, StageStatus};
        use crate::stage::Stage;

        let outcome = HookRunOutcome {
            stages: vec![StageOutcome {
                stage: Stage::PreCommit,
                status: StageStatus::Ran,
                before: Vec::new(),
                hooks: vec![outcome_with("a", HookStatus::Passed)],
                after: Vec::new(),
            }],
        };
        let mut report = String::new();
        HookRunReporter::render_legend(&mut report, &outcome);
        assert_eq!(report, "");
    }

    #[test]
    fn ran_stage_with_no_steps_renders_nothing() {
        use crate::model::{StageOutcome, StageStatus};
        use crate::stage::Stage;

        let outcome = StageOutcome {
            stage: Stage::PrepareCommitMsg,
            status: StageStatus::Ran,
            before: Vec::new(),
            hooks: Vec::new(),
            after: Vec::new(),
        };
        let mut report = String::new();
        HookRunReporter::render_stage(&mut report, &outcome);
        assert_eq!(report, "", "a no-op `Ran` stage must produce no output");
    }

    #[test]
    fn ran_stage_with_a_step_renders_the_stage_banner() {
        use crate::model::{HookStatus, StageOutcome, StageStatus, StepOutcome};
        use crate::stage::Stage;

        let outcome = StageOutcome {
            stage: Stage::PreCommit,
            status: StageStatus::Ran,
            before: vec![StepOutcome {
                command: "echo before".to_string(),
                status: HookStatus::Passed,
                output: Vec::new(),
            }],
            hooks: Vec::new(),
            after: Vec::new(),
        };
        let mut report = String::new();
        HookRunReporter::render_stage(&mut report, &outcome);
        assert!(report.contains("[stage] pre-commit"), "banner present: {report:?}");
        assert!(report.contains("before: echo before"), "step rendered: {report:?}");
    }
}
