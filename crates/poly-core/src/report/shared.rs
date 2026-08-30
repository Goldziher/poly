//! Shared rendering primitives: the [`Verbosity`] level every `pretty` renderer
//! threads through, the per-file debug block, and the ANSI stripper the stderr
//! echoes use. The colours themselves live in [`theme`](super::theme).

use std::fmt::Write as _;

use super::theme::Theme;
use crate::runner::RunDebug;

/// How much detail the human-oriented (`pretty`) renderers emit — an **ordered
/// level**, not a set of independent switches, so "less" is expressible and each
/// step is a superset of the one below it. `Copy` so it threads cheaply through
/// the renderers.
///
/// The ordering is `Quiet < Normal < Verbose < Debug`, and the renderers ask
/// questions of it ([`Verbosity::shows_notes`],
/// [`Verbosity::shows_finding_detail`], [`Verbosity::shows_debug_blocks`])
/// rather than matching on the variant, so a new level slots in without touching
/// every call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// One line per finding plus the summary — and nothing else. The
    /// qualification *counts* stay in the summary (a skip is never hidden); what
    /// goes away is the per-file discovery and skip detail beneath it.
    Quiet,
    /// The default: findings, the qualified summary, and the discovery/skip
    /// notes, each capped so a bulk reason cannot bury a rare one.
    #[default]
    Normal,
    /// Adds `description`, `url` and `metadata` to every finding, and lifts the
    /// cap on the skip note so every skipped file is named.
    Verbose,
    /// Everything [`Verbosity::Verbose`] shows, plus the dim per-file debug block
    /// (engine version, cache hit/miss, timing).
    Debug,
}

impl Verbosity {
    /// Construct a [`Verbosity`] from the historical pair of flags, where
    /// `debug` outranks `verbose`.
    ///
    /// Retained so callers written against the two-bool shape keep compiling;
    /// new code should prefer [`Verbosity::from_flags`], which can also express
    /// [`Verbosity::Quiet`].
    pub fn new(verbose: bool, debug: bool) -> Self {
        Self::from_flags(false, verbose, debug)
    }

    /// Construct a [`Verbosity`] from the three CLI flags, highest wins.
    ///
    /// `--quiet` conflicts with the other two at the clap level, so the
    /// precedence here only ever settles combinations the CLI already rejected.
    pub fn from_flags(quiet: bool, verbose: bool, debug: bool) -> Self {
        match (quiet, verbose, debug) {
            (_, _, true) => Self::Debug,
            (_, true, _) => Self::Verbose,
            (true, _, _) => Self::Quiet,
            _ => Self::Normal,
        }
    }

    /// Whether the discovery and skip *detail* notes are shown.
    ///
    /// False only under [`Verbosity::Quiet`], and it never affects the summary
    /// line: the counts and reasons live there and are reported at every level.
    pub fn shows_notes(self) -> bool {
        self > Self::Quiet
    }

    /// Whether each finding renders its `description`, `url` and `metadata`, and
    /// whether the skip note names every file instead of a capped sample.
    pub fn shows_finding_detail(self) -> bool {
        self >= Self::Verbose
    }

    /// Whether the dim per-file debug block is rendered.
    pub fn shows_debug_blocks(self) -> bool {
        self >= Self::Debug
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
        let _ = writeln!(out, "      {}", Theme::STDOUT.secondary(line));
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
