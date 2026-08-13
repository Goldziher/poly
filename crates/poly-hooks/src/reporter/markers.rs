//! Every glyph the hook report can print, declared in one place.
//!
//! Separate because these markers are defined by contrast rather than in
//! isolation: `-` (skipped), `⧖` (killed), `?` (unknown), `⋯` (running) and `⏸`
//! (queued) exist to stop one state being read as a neighbouring one, and that
//! property is only checkable when the whole set sits side by side. Every
//! renderer — the final report in [`super`], the live notices in
//! `super::notice` — takes its glyphs from here instead of spelling them
//! inline, so a new state cannot silently reuse a glyph that already means
//! something else.

use owo_colors::OwoColorize as _;

/// Return a coloured pass/fail status marker: "✓" (green) or "×" (red).
#[must_use]
pub fn project_status_marker(failed: bool) -> String {
    if failed {
        "×".red().to_string()
    } else {
        "✓".green().to_string()
    }
}

/// Marker for a hook a precondition (or an empty file set) withheld — benign.
pub(super) const SKIPPED_MARKER: &str = "-";

/// Marker for the "nothing was validated" summary line.
pub(super) const NOTHING_VALIDATED_MARKER: &str = "!";

/// Marker for a hook still executing, used by the live still-running notice.
/// Never a final status — its whole job is to be distinguishable from
/// [`SKIPPED_MARKER`], because "has not finished" and "did not apply" were the
/// two facts a single `-` used to blur together.
pub(super) const RUNNING_MARKER: &str = "⋯";

/// Marker for a hook poly killed for overrunning its budget.
pub(super) const TIMED_OUT_MARKER: &str = "⧖";

/// Marker for a hook that is quiet because it is **queued** behind a lock poly
/// does not own, not because it is working.
///
/// Distinct from [`RUNNING_MARKER`] on purpose: a hook waiting on cargo's
/// package-cache lock and a hook grinding through a build are both silent, and a
/// shared glyph would leave the reader to guess which one they are watching.
pub(super) const LOCK_WAIT_MARKER: &str = "⏸";

/// Marker for a hook whose verdict is unknown because its setup failed. Distinct
/// from both `✓/×` (a real verdict) and `-` (a benign skip).
pub(super) fn unknown_marker() -> String {
    "?".yellow().to_string()
}

/// Marker for a hook poly killed — a failure, but not the tool's own.
pub(super) fn timed_out_marker() -> String {
    TIMED_OUT_MARKER.red().to_string()
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
    fn skipped_and_timed_out_markers_are_not_the_same_glyph() {
        assert_eq!(SKIPPED_MARKER, "-");
        assert_eq!(TIMED_OUT_MARKER, "⧖");
        assert_eq!(RUNNING_MARKER, "⋯");
        assert_ne!(SKIPPED_MARKER, TIMED_OUT_MARKER);
        assert_ne!(SKIPPED_MARKER, RUNNING_MARKER);
    }
}
