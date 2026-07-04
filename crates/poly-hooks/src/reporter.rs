//! Result rendering helpers for hook execution output.
//!
//! Ported from `polyhooks/src/cli/run/reporter.rs`. Only the standalone
//! utilities that do not depend on the hook model, workspace, or indicatif
//! progress bars are ported in this phase:
//!
//! - [`project_status_marker`] — a coloured "✓" / "×" string.
//! - [`OutputPreview`] — a rolling preview buffer for streamed command output.
//! - [`truncate_to_width`] — unicode-aware ellipsis truncation.
//!
//! Live progress is emitted by [`report_hook_started`] / [`report_hook_finished`]
//! — line-oriented stderr updates the runner calls as each hook starts and
//! finishes, so a long-running tool is visibly running rather than looking hung.
//! A richer indicatif progress-bar UI (with the [`OutputPreview`] window) remains
//! a possible future upgrade but is not required for that feedback.

use std::borrow::Cow;
use std::io::Write as _;
use std::time::Duration;

use console::strip_ansi_codes;
use owo_colors::{OwoColorize as _, Stream::Stderr};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum number of lines shown in the live output preview while a hook runs.
pub const HOOK_OUTPUT_PREVIEW_LINES: usize = 3;

/// Prefix rendered before each preview line in the progress UI.
pub const HOOK_OUTPUT_PREVIEW_PREFIX: &str = "    => ";

// ── Standalone helpers ────────────────────────────────────────────────────────

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

// ── OutputPreview ─────────────────────────────────────────────────────────────

/// Rolling text preview for a running hook's streamed output.
///
/// Maintains up to [`HOOK_OUTPUT_PREVIEW_LINES`] visible lines. ANSI escape
/// codes are stripped. A pending carriage return is either joined with the
/// next `\n` (CRLF) or clears the current line to emulate terminal overwrite
/// output.
#[derive(Debug, Default)]
pub struct OutputPreview {
    lines: Vec<String>,
    line_open: bool,
    pending_cr: bool,
}

impl OutputPreview {
    /// Feed a raw output chunk into the preview state.
    ///
    /// `chunk` may contain partial lines, CRLF sequences, ANSI codes, and
    /// arbitrary binary. Non-printable / non-whitespace control characters
    /// are silently dropped.
    pub fn push_chunk(&mut self, chunk: &[u8]) {
        let text = String::from_utf8_lossy(chunk);
        let text = strip_ansi_codes(&text);
        for ch in text.chars().filter(|&c| is_preview_char(c)) {
            if self.pending_cr {
                if ch == '\n' {
                    self.finish_line();
                    self.pending_cr = false;
                    continue;
                }
                self.current_line_mut().clear();
                self.pending_cr = false;
            }
            match ch {
                '\n' => self.finish_line(),
                '\r' => self.pending_cr = true,
                '\t' => self.current_line_mut().push(' '),
                ch => self.current_line_mut().push(ch),
            }
        }
    }

    /// Return the current visible window of lines.
    ///
    /// Contains at most [`HOOK_OUTPUT_PREVIEW_LINES`] entries.
    #[must_use]
    pub fn visible_lines(&self) -> &[String] {
        &self.lines
    }

    fn current_line_mut(&mut self) -> &mut String {
        if !self.line_open {
            self.lines.push(String::new());
            self.line_open = true;
            self.truncate();
        }
        let idx = self.lines.len() - 1;
        &mut self.lines[idx]
    }

    fn finish_line(&mut self) {
        if self.line_open {
            self.line_open = false;
        } else {
            self.lines.push(String::new());
            self.truncate();
        }
    }

    fn truncate(&mut self) {
        if self.lines.len() > HOOK_OUTPUT_PREVIEW_LINES {
            let overflow = self.lines.len() - HOOK_OUTPUT_PREVIEW_LINES;
            self.lines.drain(..overflow);
        }
    }
}

fn is_preview_char(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\t') || !ch.is_control()
}

// ── CaptureSink ───────────────────────────────────────────────────────────────

/// An [`OutputSink`](crate::process::OutputSink) that accumulates every chunk
/// into a single buffer.
///
/// Each hook (and each `ARG_MAX` batch) executes with its own `CaptureSink`, so
/// concurrently-running hooks never interleave their output. The runner renders
/// the captured buffers sequentially afterwards (capture-then-render).
#[derive(Debug, Default)]
pub struct CaptureSink {
    buffer: Vec<u8>,
}

impl CaptureSink {
    /// Consume the sink and return the captured bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buffer
    }
}

impl crate::process::OutputSink for CaptureSink {
    fn write_chunk(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }
}

// ── Live progress ─────────────────────────────────────────────────────────────

/// At or above this many seconds a duration renders as `s` (e.g. `1.2s`); below
/// it, as whole milliseconds (e.g. `340ms`).
const SECS_DISPLAY_THRESHOLD: f64 = 1.0;

/// Format a hook duration compactly: `1.2s` at or above a second, else `340ms`.
fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs_f64();
    if secs >= SECS_DISPLAY_THRESHOLD {
        format!("{secs:.1}s")
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// The pass/fail marker for a live progress line, coloured against **stderr**
/// (where progress is written) so it honours `NO_COLOR` and a non-TTY stderr —
/// unlike [`project_status_marker`], which targets the stdout report.
fn progress_marker(failed: bool) -> String {
    if failed {
        "×".if_supports_color(Stderr, |t| t.red()).to_string()
    } else {
        "✓".if_supports_color(Stderr, |t| t.green()).to_string()
    }
}

/// Announce (to stderr) that a hook is now executing.
///
/// Called just before a hook runs, so a long-running tool (`cargo clippy`,
/// `cargo test`, …) is visibly *running* rather than leaving the terminal blank
/// — which reads as a hung commit. The coloured marker is materialized before
/// the stderr lock is taken, so each `writeln!` is one locked, atomic line (safe
/// under the rayon pool) and never interleaves with the captured stdout report.
pub fn report_hook_started(id: &str) {
    let marker = "▶".if_supports_color(Stderr, |t| t.cyan()).to_string();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "  {marker} {id} …");
}

/// Announce (to stderr) that a hook finished, with its pass/fail mark and how
/// long it took. Pairs with [`report_hook_started`].
pub fn report_hook_finished(id: &str, failed: bool, duration: Duration) {
    let marker = progress_marker(failed);
    let elapsed = format!("({})", format_duration(duration))
        .if_supports_color(Stderr, |t| t.dimmed())
        .to_string();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "  {marker} {id} {elapsed}");
}

// ── HookRunReporter ─────────────────────────────────────────────────────────

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
    // `&self` today carries no render options; it is kept for forward-compat
    // (colour / verbosity toggles) without a breaking signature change.
    #[allow(clippy::unused_self)]
    #[must_use]
    pub fn render(&self, outcome: &crate::model::HookRunOutcome) -> String {
        let mut report = String::new();
        for stage in &outcome.stages {
            Self::render_stage(&mut report, stage);
        }
        report
    }

    fn render_stage(report: &mut String, stage: &crate::model::StageOutcome) {
        use std::fmt::Write as _;

        use crate::model::{HookStatus, StageStatus};

        let _ = writeln!(report, "[stage] {}", stage.stage);
        match &stage.status {
            StageStatus::Skipped(reason) => {
                let _ = writeln!(report, "  skipped: {reason}");
                return;
            }
            StageStatus::Aborted(reason) => {
                let _ = writeln!(report, "  aborted: {reason}");
            }
            StageStatus::Ran => {}
        }

        for step in &stage.before {
            let _ = writeln!(
                report,
                "  {} before: {}",
                project_status_marker(step.status.is_failure()),
                step.command
            );
            append_failure_output(report, &step.status, &step.output);
        }

        for hook in &stage.hooks {
            let marker = match &hook.status {
                HookStatus::Skipped(_) => "-".to_string(),
                status => project_status_marker(status.is_failure()),
            };
            let suffix = if hook.files_modified {
                " (files modified)"
            } else if hook.cached {
                " (cached)"
            } else {
                ""
            };
            let _ = writeln!(report, "  {marker} {}{suffix}", hook.id);
            append_failure_output(report, &hook.status, &hook.output);
        }

        for step in &stage.after {
            let _ = writeln!(
                report,
                "  {} after: {}",
                project_status_marker(step.status.is_failure()),
                step.command
            );
            append_failure_output(report, &step.status, &step.output);
        }
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

    #[test]
    fn output_preview_collects_lines() {
        let mut p = OutputPreview::default();
        p.push_chunk(b"line1\nline2\nline3\n");
        assert_eq!(p.visible_lines(), ["line1", "line2", "line3"]);
    }

    #[test]
    fn output_preview_caps_at_max_lines() {
        let mut p = OutputPreview::default();
        for i in 0..10 {
            p.push_chunk(format!("line{i}\n").as_bytes());
        }
        assert!(p.visible_lines().len() <= HOOK_OUTPUT_PREVIEW_LINES);
    }

    #[test]
    fn output_preview_cr_clears_line() {
        let mut p = OutputPreview::default();
        p.push_chunk(b"old\rnew\n");
        assert_eq!(p.visible_lines(), ["new"]);
    }

    #[test]
    fn output_preview_crlf_treated_as_newline() {
        let mut p = OutputPreview::default();
        p.push_chunk(b"a\r\nb\n");
        assert_eq!(p.visible_lines(), ["a", "b"]);
    }

    #[test]
    fn output_preview_strips_ansi_codes() {
        let mut p = OutputPreview::default();
        p.push_chunk(b"\x1b[31mred\x1b[0m\n");
        assert_eq!(p.visible_lines(), ["red"]);
    }
}
