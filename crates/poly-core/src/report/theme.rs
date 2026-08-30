//! poly's colour palette, in one place, named by meaning.
//!
//! Colour usage used to be per-surface: every renderer reached for `.red()` or
//! `.yellow()` inline, so "what colour is a skip?" had as many answers as there
//! were call sites. Here a call site asks for [`Theme::skipped`], not for a
//! colour, which is what lets the palette change without a sweep through the
//! renderers — and what keeps two surfaces from disagreeing about what yellow
//! means.
//!
//! Three rules hold this together:
//!
//! - **The basic 16 ANSI colours only.** They are resolved by the user's own
//!   terminal theme, so they stay legible on light and dark backgrounds alike; a
//!   256-colour or truecolor value assumes a background poly cannot see.
//! - **Colour is never the only carrier of meaning.** Every line that is
//!   coloured also says what it is in words (`error`, `skipped`, `ok:`), so a
//!   `--no-color` run, a pipe, and a CI log lose decoration and nothing else.
//! - **Everything goes through `if_supports_color`**, which honours `--no-color`
//!   (via `owo_colors::set_override`), `NO_COLOR`, and TTY detection — resolved
//!   against the stream the text is actually written to, so a piped stdout with
//!   a terminal stderr colours each correctly.

use owo_colors::{OwoColorize, Stream};

use crate::engine::Severity;

/// The palette, bound to the stream its output will be written to.
///
/// `Copy`, so renderers thread it through without ceremony.
#[derive(Debug, Clone, Copy)]
pub struct Theme(Stream);

impl Theme {
    /// The palette for text written to stdout — the reports themselves.
    pub const STDOUT: Self = Self(Stream::Stdout);
    /// The palette for text written to stderr — the notes and echoes that must
    /// not corrupt a `--format json` document on stdout.
    pub const STDERR: Self = Self(Stream::Stderr);

    /// A section heading or the path a block of findings belongs to: bold, no
    /// hue, so it separates without competing with the severity colours.
    pub fn heading(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.bold()).to_string()
    }

    /// A file path named inside a line of prose.
    pub fn path(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.cyan()).to_string()
    }

    /// A rule id, engine code, or other machine identifier: de-emphasised, since
    /// the reader scans for the message first and looks the code up second.
    pub fn code(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.dimmed()).to_string()
    }

    /// The engine that produced a finding.
    pub fn engine(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.magenta()).to_string()
    }

    /// Secondary detail: timings, continuation lines, anything the reader may
    /// skip without losing the verdict.
    pub fn secondary(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.dimmed()).to_string()
    }

    /// A clean result, a passing check, a completed fix.
    pub fn success(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.green()).to_string()
    }

    /// A failing check, an unusable file, a verdict the caller must act on.
    pub fn failure(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.red()).to_string()
    }

    /// Work poly declined or could not reach: skips, exclusions, files nothing
    /// recognised. Shares warning's yellow deliberately — an unexamined file is
    /// a qualification on the result, which is exactly what a warning is.
    pub fn skipped(self, text: impl std::fmt::Display) -> String {
        text.if_supports_color(self.0, |t| t.yellow()).to_string()
    }

    /// A diagnostic's severity label. The four mappings are long-established and
    /// read at a glance; nothing else in the palette may claim them for another
    /// meaning.
    pub fn severity(self, severity: Severity) -> String {
        match severity {
            Severity::Error => "error".if_supports_color(self.0, |t| t.red()).to_string(),
            Severity::Warning => "warning".if_supports_color(self.0, |t| t.yellow()).to_string(),
            Severity::Info => "info".if_supports_color(self.0, |t| t.blue()).to_string(),
            Severity::Hint => "hint".if_supports_color(self.0, |t| t.cyan()).to_string(),
        }
    }
}
