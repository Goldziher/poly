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
//! The full `HookRunReporter` (indicatif / progress-bar UI) is left as a
//! **B1 TODO** — it requires the `Hook` model and the rayon runner, which are
//! introduced in the next phase.

use std::borrow::Cow;

use console::strip_ansi_codes;
use owo_colors::OwoColorize as _;
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

// ── HookRunReporter (B1 TODO) ─────────────────────────────────────────────────

// TODO(B1): Port `HookRunReporter` once the `Hook` model and rayon runner are
// available. The full implementation requires:
//   - `crate::hook::Hook` (rayon hook-runner phase)
//   - `indicatif` progress bars + `MultiProgress`
//   - Per-hook `HookBar` and `HookGroup` tracking structs
//   - `HookOutputSink` implementing `OutputSink`

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
