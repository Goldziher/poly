//! Flags every `poly` subcommand accepts (`global = true` on the top-level parser).
//!
//! `main` parses them once and [`GlobalArgs::install`]s them, so a handler reads
//! [`global`] instead of threading a struct through every call. Reading before
//! install yields the defaults, which is what library callers and unit tests get.

use std::path::PathBuf;
use std::sync::OnceLock;

use clap::{Args, ValueEnum};

/// When to colour output (`--color`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Colour only when stdout is a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Always colour, even when piped.
    Always,
    /// Never colour.
    Never,
}

/// What a run does when a file needs an engine component that is not installed
/// (`--on-missing`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnMissing {
    /// Exit 2.
    Fail,
    /// Skip the file and count it as skipped.
    Skip,
}

impl From<OnMissing> for poly_config::v2::OnMissing {
    fn from(value: OnMissing) -> Self {
        match value {
            OnMissing::Fail => Self::Fail,
            OnMissing::Skip => Self::Skip,
        }
    }
}

/// The global flags.
#[derive(Args, Clone, Debug, Default)]
pub struct GlobalArgs {
    /// Path to a config file (default: nearest poly.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// When to colour output. `auto` honours `NO_COLOR` and TTY detection.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    pub color: ColorChoice,

    /// Never use the network: no component, grammar, catalog-tool, remote
    /// `extends` or hook-source fetches. Also set by `POLY_OFFLINE=1`.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Trim `pretty` output to the findings and the summary: drop the per-file
    /// discovery and skip detail printed beneath it.
    ///
    /// Every count and reason stays — the summary still reports how many files
    /// were linted, how many were skipped and why, and what discovery excluded.
    /// Only the itemised list of paths goes away. Findings still print and exit
    /// codes are unchanged. No effect on the `--format json`/`toon` document.
    /// Also silences poly's info-level notices on stderr (a recipe tool's
    /// download and run progress); warnings still print.
    /// Mutually exclusive with `--verbose` and `--debug`.
    #[arg(short = 'q', long, global = true, conflicts_with_all = ["verbose", "debug"])]
    pub quiet: bool,

    /// Show extra per-finding detail in `pretty` output: description, rule URL,
    /// and metadata, and list every skipped file instead of the first few. For
    /// `--format json`/`toon` the findings are always fully structured, so this
    /// only lifts the cap on the skip note printed to stderr.
    #[arg(short = 'v', long, global = true)]
    pub verbose: bool,

    /// Emit debug data: per-engine cache hit/miss and timing (shown in `pretty`,
    /// attached to `json`/`toon`), and raise log verbosity to `debug` on stderr.
    ///
    /// The top of the verbosity ladder, so it implies `--verbose`.
    #[arg(long, global = true)]
    pub debug: bool,
}

static GLOBAL: OnceLock<GlobalArgs> = OnceLock::new();

impl GlobalArgs {
    /// Record these flags for the rest of the process. First call wins.
    pub fn install(self) {
        poly_config::network::set_offline(self.offline);
        let _ = GLOBAL.set(self);
    }
}

/// The installed global flags, or the defaults when none were installed.
#[must_use]
pub fn global() -> &'static GlobalArgs {
    static DEFAULT: OnceLock<GlobalArgs> = OnceLock::new();
    GLOBAL.get().unwrap_or_else(|| DEFAULT.get_or_init(GlobalArgs::default))
}

/// Whether the run is offline (`--offline` or `POLY_OFFLINE`).
#[must_use]
pub fn offline() -> bool {
    global().offline || poly_config::network::offline()
}
