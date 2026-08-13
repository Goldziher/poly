//! Result rendering helpers for hook execution output.
//!
//! Ported from `polyhooks/src/cli/run/reporter.rs`. The families live in this
//! module and its children:
//!
//! - **Final render** (this module) — [`HookRunReporter`] turns a completed
//!   [`HookRunOutcome`](crate::model::HookRunOutcome) into a deterministic,
//!   non-interleaved report, on top of the shared text helpers
//!   [`truncate_to_width`] and `format_duration`.
//! - **Marker vocabulary** (`markers`) — every glyph the report can print,
//!   declared together so no two states end up sharing one.
//! - **Failure annotations** (`failure`) — what poly, rather than the hook,
//!   has to add to a failure: a fix it withheld, or a verdict reached against
//!   the staged snapshot.
//! - **Live notices** (`notice`) — the [`still_running_line`] /
//!   [`lock_wait_line`] vocabulary printed while hooks are in flight.
//! - **Live progress** ([`progress`]) — the spinner UI, the rolling
//!   [`OutputPreview`] window, and the output sinks that feed them.
//!
//! Everything callers use is re-exported here, so the module split is invisible
//! to them.
//!
//! The report's job is to keep distinct states distinct: a hook that was
//! skipped, one poly killed, one whose setup failed, and one whose fix could not
//! be delivered all get their own marker and their own sentence, because each
//! asks the reader for something different.

use std::borrow::Cow;
use std::time::Duration;

use console::strip_ansi_codes;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

mod failure;
mod markers;
mod notice;
pub mod progress;

use failure::{append_isolation_hint, append_withheld_fix, is_isolation_hint_eligible};
use markers::{NOTHING_VALIDATED_MARKER, SKIPPED_MARKER, timed_out_marker, unknown_marker};

pub use markers::project_status_marker;
pub use notice::{lock_wait_line, still_running_line};
pub use progress::{
    CaptureSink, HOOK_OUTPUT_PREVIEW_LINES, HOOK_OUTPUT_PREVIEW_PREFIX, HookBar, OutputPreview, PreviewSink, ProgressUi,
};

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
        // Tracks whether the staged-isolation hint has already been printed this
        // run, so a run with several staged-validated failures explains itself
        // once — at the first one — instead of repeating the same aside after
        // every failure and becoming noise in its own right.
        let mut isolation_hint_shown = false;
        for stage in &outcome.stages {
            Self::render_stage(&mut report, stage, &mut isolation_hint_shown);
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

    fn render_stage(report: &mut String, stage: &crate::model::StageOutcome, isolation_hint_shown: &mut bool) {
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
            Self::render_hook(report, hook, isolation_hint_shown);
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
    fn render_hook(report: &mut String, hook: &crate::model::HookOutcome, isolation_hint_shown: &mut bool) {
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
        if !*isolation_hint_shown && is_isolation_hint_eligible(hook) {
            append_isolation_hint(report);
            *isolation_hint_shown = true;
        }
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
    ///
    /// `pub(super)` because the render tests that need it are split across this
    /// module's children (see `failure`), and every one of them has to build the
    /// same shape of outcome.
    pub(super) fn outcome_with(id: &str, status: crate::model::HookStatus) -> crate::model::HookOutcome {
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
        HookRunReporter::render_hook(&mut report, &hook, &mut false);
        assert_eq!(
            report,
            format!(
                "  {} ai-rulez:ai-rulez-validate (timed out: poly killed it after 600.4s, limit 600.0s)\n",
                timed_out_marker()
            )
        );
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
        HookRunReporter::render_stage(&mut report, &outcome, &mut false);
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
        HookRunReporter::render_stage(&mut report, &outcome, &mut false);
        assert!(report.contains("[stage] pre-commit"), "banner present: {report:?}");
        assert!(report.contains("before: echo before"), "step rendered: {report:?}");
    }
}
