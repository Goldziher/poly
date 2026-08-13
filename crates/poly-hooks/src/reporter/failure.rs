//! The annotations a failing hook carries *beyond* the tool's own output.
//!
//! Separate because these lines are poly explaining itself, not the hook
//! speaking. A fix poly computed and then refused to write, and a verdict
//! reached against the staged snapshot rather than the worktree, are both facts
//! the failing tool has no way to report — without them the reader sees either
//! a failure with no output at all, or output that contradicts what a local run
//! just told them. They also answer to a stricter editorial rule than the rest
//! of the render: every line here must leave the reader with something to do,
//! and must be printed only when it is actually true of the failure at hand.

/// Label leading a withheld-fix line poly declined for **safety**, not for the
/// author's convenience: a symlink destination or a path leaving the repository.
///
/// Shouted, and never applied to the unstaged-changes case, because the two ask
/// the reader for entirely different things and only one of them is a finding
/// about the repository's contents.
pub(super) const SECURITY_REFUSAL_LABEL: &str = "SECURITY:";

/// Spell out a withheld fix: which files, why each one was withheld, and what
/// the author has to do about it. The hook exited 0, so without this the report
/// would show a failure with no output at all.
///
/// Each path carries its own reason. Rendering them all as "unstaged changes"
/// — as this used to — is wrong for every case but one, and for a security
/// refusal it is worse than wrong: it sends the author to `git add` a symlink
/// poly will never write through, when what the tree actually contains is a
/// path that escapes the repository.
pub(super) fn append_withheld_fix(report: &mut String, withheld: &[crate::model::WithheldFix]) {
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

/// `true` for a failure that is worth explaining as "checked the staged
/// snapshot, not your working tree": the hook actually ran under isolation and
/// reported a real verdict about the content it read.
///
/// Deliberately narrower than [`crate::model::HookStatus::is_failure`]. A
/// [`HookStatus::Unknown`](crate::model::HookStatus::Unknown) or
/// [`HookStatus::TimedOut`](crate::model::HookStatus::TimedOut) never judged
/// the content at all, so naming the tree it *would* have read would answer a
/// question nobody asked; [`HookStatus::FixWithheld`](crate::model::HookStatus::FixWithheld)
/// already carries its own tree-specific remedy in [`append_withheld_fix`], and
/// stacking this hint on top of that one would repeat the point in a second
/// voice instead of reinforcing it.
pub(super) fn is_isolation_hint_eligible(hook: &crate::model::HookOutcome) -> bool {
    hook.validated == crate::model::ValidatedTree::StagedIndex
        && matches!(hook.status, crate::model::HookStatus::Failed { .. })
}

/// Explain, at the first hook whose failure earns it, that the content checked
/// was poly's staged snapshot rather than the working tree — the fact a local
/// `cargo check` passing seconds earlier does not contradict this hook's
/// failure, because the two ran against different bytes.
///
/// [`crate::reporter::HookRunReporter::render`] prints this once per run, not
/// once per failing hook: repeating the same aside after every failure would
/// turn a useful hint into scroll-past noise, exactly the problem this hint
/// exists to fix.
pub(super) fn append_isolation_hint(report: &mut String) {
    use std::fmt::Write as _;

    let _ = writeln!(
        report,
        "      checked against the staged snapshot, not your working tree — a mismatch with what \
         you see locally is expected if you have unstaged changes. `git add` the rest of your \
         change, or set `[hooks] isolate = false` to validate the worktree instead."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::tests::outcome_with;
    use crate::reporter::{HookRunReporter, project_status_marker};

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
        HookRunReporter::render_hook(&mut report, &hook, &mut false);

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
        HookRunReporter::render_hook(&mut report, &hook, &mut false);

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
        HookRunReporter::render_hook(&mut report, &hook, &mut false);

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

    /// The exact wording of the staged-isolation hint, asserted once so every
    /// other test in this group can check for its presence/absence by content
    /// without re-typing it.
    const ISOLATION_HINT: &str = "      checked against the staged snapshot, not your working tree — a mismatch \
         with what you see locally is expected if you have unstaged changes. `git add` the rest of \
         your change, or set `[hooks] isolate = false` to validate the worktree instead.\n";

    /// THE BUG. A hook that failed under the staged snapshot used to print its
    /// failure output with no indication that the bytes it read were not the
    /// worktree's — the exact confusion two consumers independently hit against
    /// 0.20.0. A failure validated against the staged index must now carry the
    /// hint, attached right after its output.
    #[test]
    fn a_staged_failure_explains_it_checked_the_snapshot_not_the_worktree() {
        use crate::model::{HookStatus, ValidatedTree};

        let hook = crate::model::HookOutcome {
            validated: ValidatedTree::StagedIndex,
            ..outcome_with("clippy", HookStatus::Failed { code: Some(1) })
        };
        let mut report = String::new();
        let mut hint_shown = false;
        HookRunReporter::render_hook(&mut report, &hook, &mut hint_shown);

        assert!(hint_shown, "the flag must record that the hint was printed");
        assert_eq!(
            report,
            format!("  {} clippy\n{ISOLATION_HINT}", project_status_marker(true))
        );
    }

    /// THE OVER-BROAD IMPLEMENTATION THIS CATCHES. A worktree-validated run
    /// checked exactly what the author sees locally, so the hint would be
    /// actively wrong here — a naive "always hint on failure" would fail this.
    #[test]
    fn a_worktree_failure_carries_no_staged_isolation_hint() {
        use crate::model::HookStatus;

        let hook = outcome_with("clippy", HookStatus::Failed { code: Some(1) });
        let mut report = String::new();
        let mut hint_shown = false;
        HookRunReporter::render_hook(&mut report, &hook, &mut hint_shown);

        assert!(!hint_shown, "a worktree-validated failure must not raise the hint flag");
        assert_eq!(report, format!("  {} clippy\n", project_status_marker(true)));
    }

    /// A passing hook under staged validation has nothing to explain: the
    /// content checked matched the commit either way, so the hint would be
    /// noise on a clean run.
    #[test]
    fn a_staged_pass_carries_no_isolation_hint() {
        use crate::model::{HookStatus, ValidatedTree};

        let hook = crate::model::HookOutcome {
            validated: ValidatedTree::StagedIndex,
            ..outcome_with("clippy", HookStatus::Passed)
        };
        let mut report = String::new();
        let mut hint_shown = false;
        HookRunReporter::render_hook(&mut report, &hook, &mut hint_shown);

        assert!(!hint_shown, "a passing hook must not raise the hint flag");
        assert_eq!(report, format!("  {} clippy\n", project_status_marker(false)));
    }

    /// Several staged failures in one run must not each restate the hint — that
    /// would bury the point under the exact scroll-past noise it exists to cut
    /// through. It attaches once, at the first qualifying failure.
    #[test]
    fn several_staged_failures_in_one_run_get_the_isolation_hint_only_once() {
        use crate::model::{HookRunOutcome, HookStatus, StageOutcome, StageStatus, ValidatedTree};
        use crate::stage::Stage;

        let staged_failure = |id: &str| crate::model::HookOutcome {
            validated: ValidatedTree::StagedIndex,
            ..outcome_with(id, HookStatus::Failed { code: Some(1) })
        };
        let outcome = HookRunOutcome {
            stages: vec![StageOutcome {
                stage: Stage::PreCommit,
                status: StageStatus::Ran,
                before: Vec::new(),
                hooks: vec![staged_failure("clippy"), staged_failure("fmt")],
                after: Vec::new(),
            }],
        };
        let report = HookRunReporter::new().render(&outcome);

        assert_eq!(
            report.matches("checked against the staged snapshot").count(),
            1,
            "the hint must appear exactly once across the whole run: {report}"
        );
        let hint_pos = report
            .find("checked against the staged snapshot")
            .expect("hint present");
        let fmt_line = format!("  {} fmt", project_status_marker(true));
        let fmt_pos = report.find(&fmt_line).expect("fmt hook line present");
        assert!(
            hint_pos < fmt_pos,
            "the hint must attach to the first failure, before the second hook's line: {report}"
        );
    }
}
