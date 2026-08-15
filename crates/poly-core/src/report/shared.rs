//! Shared rendering primitives: the [`Verbosity`] type every `pretty` renderer
//! threads through, the colored severity label, the per-file debug block, and
//! the ANSI stripper the stderr echoes use.

use std::fmt::Write as _;

use owo_colors::{OwoColorize, Stream::Stdout};

use crate::engine::Severity;
use crate::runner::RunDebug;

/// How much detail the human-oriented (`pretty`) renderers emit. `Copy` so it
/// threads cheaply through the renderers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Verbosity {
    /// Show `description`, `url`, and `metadata` for each finding.
    pub verbose: bool,
    /// Show the per-file debug block (engine version, cache hit/miss, timing).
    pub debug: bool,
}

impl Verbosity {
    /// Construct a [`Verbosity`] from the two flags.
    pub fn new(verbose: bool, debug: bool) -> Self {
        Self { verbose, debug }
    }
}

/// Format the colored severity label for a diagnostic.
pub(super) fn severity_label(severity: Severity) -> String {
    match severity {
        Severity::Error => "error".if_supports_color(Stdout, |t| t.red()).to_string(),
        Severity::Warning => "warning".if_supports_color(Stdout, |t| t.yellow()).to_string(),
        Severity::Info => "info".if_supports_color(Stdout, |t| t.blue()).to_string(),
        Severity::Hint => "hint".if_supports_color(Stdout, |t| t.cyan()).to_string(),
    }
}

/// Render the dim per-file debug block (engine version, cache hit/miss, timing).
pub(super) fn render_debug_block(out: &mut String, debug: &RunDebug) {
    for e in &debug.engines {
        let status = if e.cache_hit { "cache hit" } else { "ran" };
        let line = format!(
            "[debug] {} v{}  {}  {:.2}ms",
            e.engine, e.version, status, e.duration_ms
        );
        let _ = writeln!(out, "      {}", line.if_supports_color(Stdout, |t| t.dimmed()));
    }
}

/// Remove ANSI SGR sequences from `text`.
///
/// Only ever applied to poly's own rendered notes, which contain nothing more
/// exotic than `ESC [ … m`.
pub(super) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for escape in chars.by_ref() {
            if escape == 'm' {
                break;
            }
        }
    }
    out
}
