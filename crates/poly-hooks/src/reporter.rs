//! Result rendering helpers for hook execution output.
//!
//! Ported from `polyhooks/src/cli/run/reporter.rs`. Two families live here:
//!
//! - **Final render** — [`HookRunReporter`] turns a completed
//!   [`HookRunOutcome`](crate::model::HookRunOutcome) into a deterministic,
//!   non-interleaved report, with the standalone helpers
//!   [`project_status_marker`], [`OutputPreview`], and [`truncate_to_width`].
//! - **Live progress** — [`ProgressUi`] wraps an [`indicatif::MultiProgress`]:
//!   each executing hook gets a spinner ([`HookBar`]) whose message shows a
//!   rolling [`OutputPreview`] window fed by a [`PreviewSink`], collapsing to a
//!   persistent `✓/× id (dur)` line on completion. It is enabled only when a run
//!   requests progress (an interactive stderr) and self-hides on a non-terminal,
//!   so the deterministic final report is unaffected.

use std::borrow::Cow;
use std::time::Duration;

use console::strip_ansi_codes;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use owo_colors::{OwoColorize as _, Stream::Stderr};
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use crate::process::OutputSink;

/// Maximum number of lines shown in the live output preview while a hook runs.
pub const HOOK_OUTPUT_PREVIEW_LINES: usize = 3;

/// Prefix rendered before each preview line in the progress UI.
pub const HOOK_OUTPUT_PREVIEW_PREFIX: &str = "    => ";

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

/// An [`OutputSink`] that accumulates every chunk into a single buffer.
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

/// Spinner redraw cadence. indicatif drives the animation from its own ticker
/// thread, so a hook blocked in a subprocess still animates.
const SPINNER_TICK_MS: u64 = 90;

/// Braille spinner frames (trailing space is the "done" frame indicatif lands on
/// after `finish`).
const SPINNER_FRAMES: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ ";

/// Fallback preview width when the terminal size cannot be probed.
const DEFAULT_PREVIEW_WIDTH: usize = 100;

/// A live multi-line progress display for a hook run.
///
/// Wraps an [`indicatif::MultiProgress`] drawn to stderr. Each executing hook
/// gets a spinner line (via [`Self::start`]) whose message is updated with a
/// rolling [`OutputPreview`] window as the tool streams output; on completion
/// [`Self::finish`] clears the spinner and prints a persistent `✓/× id (dur)`
/// line above the still-running bars.
///
/// The draw target hides itself on a non-terminal stderr, so it is safe to
/// construct whenever live progress is requested. `MultiProgress` is `Send +
/// Sync`, so a shared `&ProgressUi` is used directly inside the rayon pool.
#[derive(Debug)]
pub struct ProgressUi {
    multi: MultiProgress,
}

impl ProgressUi {
    /// Create a stderr-backed progress display.
    #[must_use]
    pub fn new() -> Self {
        Self {
            multi: MultiProgress::with_draw_target(ProgressDrawTarget::stderr()),
        }
    }

    /// Start a spinner for a hook that is about to execute.
    #[must_use]
    pub fn start(&self, id: &str) -> HookBar {
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(spinner_style());
        bar.enable_steady_tick(Duration::from_millis(SPINNER_TICK_MS));
        bar.set_message(format!("{id} …"));
        HookBar {
            bar,
            id: id.to_string(),
        }
    }

    /// Finish a hook's spinner: clear the live line and print a persistent
    /// `✓/× id (dur)` result above the remaining bars.
    pub fn finish(&self, hook_bar: &HookBar, failed: bool, duration: Duration) {
        hook_bar.bar.finish_and_clear();
        let marker = progress_marker(failed);
        let elapsed = format!("({})", format_duration(duration))
            .if_supports_color(Stderr, |t| t.dimmed())
            .to_string();
        let _ = self.multi.println(format!("  {marker} {} {elapsed}", hook_bar.id));
    }
}

impl Default for ProgressUi {
    fn default() -> Self {
        Self::new()
    }
}

/// A single hook's spinner handle, returned by [`ProgressUi::start`].
#[derive(Debug)]
pub struct HookBar {
    bar: ProgressBar,
    id: String,
}

impl HookBar {
    /// The underlying spinner, so a [`PreviewSink`] can update its message.
    #[must_use]
    pub fn bar(&self) -> &ProgressBar {
        &self.bar
    }
}

/// The braille spinner style: a cyan spinner followed by the (possibly
/// multi-line) preview message.
fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .tick_chars(SPINNER_FRAMES)
}

/// Probe the terminal width for truncating preview lines; falls back to a fixed
/// width when stderr is not a sizeable terminal.
fn preview_width() -> usize {
    console::Term::stderr()
        .size_checked()
        .map_or(DEFAULT_PREVIEW_WIDTH, |(_, cols)| {
            (cols as usize).saturating_sub(HOOK_OUTPUT_PREVIEW_PREFIX.len())
        })
}

/// Build the spinner message: the hook id on the first line, then up to
/// [`HOOK_OUTPUT_PREVIEW_LINES`] rolling preview lines, each truncated to the
/// terminal width so the multi-line layout never wraps.
fn preview_message(id: &str, preview: &OutputPreview, width: usize) -> String {
    let lines = preview.visible_lines();
    if lines.is_empty() {
        return format!("{id} …");
    }
    let mut message = id.to_string();
    for line in lines {
        message.push('\n');
        message.push_str(HOOK_OUTPUT_PREVIEW_PREFIX);
        message.push_str(&truncate_to_width(line, width));
    }
    message
}

/// An [`OutputSink`] that captures every byte (for the deterministic final
/// render) **and** drives a live [`OutputPreview`] into a spinner's message.
///
/// Each hook (and each `ARG_MAX` batch) owns its own sink; batches that share a
/// hook's spinner update its message last-writer-wins, which is fine for a
/// best-effort preview.
#[derive(Debug)]
pub struct PreviewSink<'a> {
    buffer: Vec<u8>,
    preview: OutputPreview,
    bar: &'a ProgressBar,
    width: usize,
    id: &'a str,
}

impl<'a> PreviewSink<'a> {
    /// Create a preview sink that updates `bar` with output streamed for `id`.
    #[must_use]
    pub fn new(bar: &'a ProgressBar, id: &'a str) -> Self {
        Self {
            buffer: Vec::new(),
            preview: OutputPreview::default(),
            bar,
            width: preview_width(),
            id,
        }
    }

    /// Consume the sink and return the fully captured bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buffer
    }
}

impl OutputSink for PreviewSink<'_> {
    fn write_chunk(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
        self.preview.push_chunk(chunk);
        self.bar
            .set_message(preview_message(self.id, &self.preview, self.width));
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
        let mut entries: Vec<String> = Vec::new();
        if statuses().any(|hook| matches!(hook.status, HookStatus::Skipped(_))) {
            entries.push(format!("{SKIPPED_MARKER} skipped (did not apply)"));
        }
        if statuses().any(|hook| matches!(hook.status, HookStatus::TimedOut(_))) {
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
            let _ = writeln!(
                report,
                "  {} before: {}",
                project_status_marker(step.status.is_failure()),
                step.command
            );
            append_failure_output(report, &step.status, &step.output);
        }

        for hook in &stage.hooks {
            Self::render_hook(report, hook);
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
                let _ = writeln!(report, "      before: {}", step.command);
                append_failure_output(report, &step.status, &step.output);
            }
        }
        if let HookStatus::FixWithheld(paths) = &hook.status {
            append_withheld_fix(report, paths);
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

/// Spell out a withheld fix: which files, and what the author has to do. The
/// hook exited 0, so without this the report would show a failure with no
/// output at all.
fn append_withheld_fix(report: &mut String, paths: &[std::path::PathBuf]) {
    use std::fmt::Write as _;

    let _ = writeln!(
        report,
        "      fixed the staged content, but these files have unstaged changes, \
         so the fix was not written:"
    );
    for path in paths {
        let _ = writeln!(report, "        {}", path.display());
    }
    let _ = writeln!(
        report,
        "      re-run the fixer over your working tree and `git add` the result, \
         or stash the unstaged changes first."
    );
}

/// Marker for a hook a precondition (or an empty file set) withheld — benign.
const SKIPPED_MARKER: &str = "-";

/// Marker for the "nothing was validated" summary line.
const NOTHING_VALIDATED_MARKER: &str = "!";

/// Marker for a hook still executing, used by the live still-running notice.
/// Never a final status — its whole job is to be distinguishable from
/// [`SKIPPED_MARKER`], because "has not finished" and "did not apply" were the
/// two facts a single `-` used to blur together.
const RUNNING_MARKER: &str = "⋯";

/// Marker for a hook poly killed for overrunning its budget.
const TIMED_OUT_MARKER: &str = "⧖";

/// Marker for a hook whose verdict is unknown because its setup failed. Distinct
/// from both `✓/×` (a real verdict) and `-` (a benign skip).
fn unknown_marker() -> String {
    "?".yellow().to_string()
}

/// Marker for a hook poly killed — a failure, but not the tool's own.
fn timed_out_marker() -> String {
    TIMED_OUT_MARKER.red().to_string()
}

/// The line a hook prints while it is still running, so a hang names its
/// culprit as it happens instead of leaving a silent terminal.
///
/// Written to stderr (or above the live spinners), never to the final report.
/// Naming the budget alongside the elapsed time answers the reader's actual
/// question — "is this thing ever going to stop?" — rather than only "it is
/// slow".
#[must_use]
pub fn still_running_line(id: &str, elapsed: Duration, limit: Option<Duration>) -> String {
    let elapsed = format_duration(elapsed);
    match limit {
        Some(limit) => format!(
            "  {RUNNING_MARKER} still running: {id} ({elapsed} elapsed, killed at {})",
            format_duration(limit)
        ),
        None => format!("  {RUNNING_MARKER} still running: {id} ({elapsed} elapsed, no timeout)"),
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

    #[test]
    fn preview_sink_captures_every_byte_across_chunks() {
        let bar = ProgressBar::hidden();
        let mut sink = PreviewSink::new(&bar, "demo");
        sink.write_chunk(b"first line\n");
        sink.write_chunk(b"\x1b[32msecond\x1b[0m\n");
        assert_eq!(sink.into_bytes(), b"first line\n\x1b[32msecond\x1b[0m\n");
    }

    #[test]
    fn preview_message_shows_id_alone_before_any_output() {
        let preview = OutputPreview::default();
        assert_eq!(preview_message("clippy", &preview, 80), "clippy …");
    }

    #[test]
    fn preview_message_appends_prefixed_rolling_lines() {
        let mut preview = OutputPreview::default();
        preview.push_chunk(b"compiling\nlinking\n");
        let message = preview_message("clippy", &preview, 80);
        let mut lines = message.lines();
        assert_eq!(lines.next(), Some("clippy"));
        assert_eq!(lines.next(), Some("    => compiling"));
        assert_eq!(lines.next(), Some("    => linking"));
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
            HookStatus::TimedOut(TimeoutReason {
                limit: Duration::from_mins(10),
                elapsed: Duration::from_secs_f64(600.4),
            }),
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
            still_running_line("clippy", Duration::from_secs(15), Some(Duration::from_mins(30))),
            "  ⋯ still running: clippy (15.0s elapsed, killed at 1800.0s)"
        );
        assert_eq!(
            still_running_line("clippy", Duration::from_millis(750), None),
            "  ⋯ still running: clippy (750ms elapsed, no timeout)"
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
                        HookStatus::TimedOut(TimeoutReason {
                            limit: Duration::from_mins(10),
                            elapsed: Duration::from_secs(601),
                        }),
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
