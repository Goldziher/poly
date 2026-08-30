//! Output rendering in three formats: `pretty` (colored, human-oriented),
//! `json` (`serde_json`), and `toon` (Token-Oriented Object Notation).
//!
//! Coloring goes through owo-colors' `if_supports_color`, which respects both
//! TTY detection and the global override set by `--no-color`. The `toon`
//! renderers fall back to JSON if only the TOON encoder fails, so output is
//! never lost; if the value cannot be serialized at all they return a
//! [`RenderError`] rather than a document, because an empty array is
//! indistinguishable from a clean run. The `pretty` renderers split into a
//! `render_*` core that produces the string and a `report_*` wrapper that prints
//! it, so the rendered text can be snapshot-tested.
//!
//! ## Verbosity contract
//!
//! [`Verbosity`] is an ordered level — `Quiet < Normal < Verbose < Debug` — and
//! selects how much of each diagnostic the `pretty` renderers show:
//! - **`--quiet`** — findings and the summary, and nothing beneath it: the
//!   per-file discovery and skip detail is dropped. The *counts* never are —
//!   the summary still names how many files were skipped and why, because a
//!   hidden skip is exactly the failure the notes exist to prevent.
//! - **default** — one terse line per finding (`level  engine  code?  line:col?
//!   title`) plus the discovery/skip notes. `description`, `url`, and
//!   `metadata` are hidden.
//! - **`--verbose`** — additionally renders `description`, `url`, and any
//!   `metadata` as indented lines, and lifts the cap on the skipped-file note.
//! - **`--debug`** — everything `--verbose` shows, plus a dim per-file debug
//!   block (engine version, cache hit/miss, timing).
//!
//! For `json` / `toon` the full structured record is **always** emitted (serde
//! omits empty/`None` fields), so `--verbose` is a no-op there; `--debug` simply
//! causes the runner to attach the `debug` field, which then serializes.
//!
//! The module is split by concern: [`theme`] holds the semantic colour palette
//! every surface draws from, `layout` the typography (grouped numbers, real
//! plurals, width-aware sample lines), `shared` the verbosity level and the
//! remaining styling primitives, `notes` the discovery/skip qualification,
//! `lint` and `format` the human-oriented renderers for each run kind, and
//! `structured` the `json` / `toon` serialization.
//!
//! ## Reading order
//!
//! Every human-oriented report ends with its verdict: findings first, then the
//! files that failed, then the qualification notes, and the summary **last**.
//! A reader's eye lands on the bottom of the output, so that is where the answer
//! goes — the earlier layout printed the headline and then buried it under
//! twenty-nine lines of skip detail.

mod format;
mod layout;
mod lint;
mod notes;
mod render;
mod shared;
mod structured;
pub mod theme;

pub use format::{
    eprint_format_errors, render_format_errors, render_format_pretty, render_format_pretty_run, report_format_pretty,
    report_format_pretty_run,
};
pub use layout::{directories, files, issues, number, paths, quantity, rules};
pub use lint::{
    eprint_lint_errors, render_lint_errors, render_lint_pretty, render_lint_pretty_run, report_lint_pretty,
    report_lint_pretty_run,
};
pub use notes::{eprint_discovery_note, eprint_skip_note, render_discovery_note, render_skip_note};
pub use render::{RenderError, render_json, render_toon};
pub use shared::Verbosity;
pub use structured::{
    report_format_json, report_format_json_run, report_format_toon, report_format_toon_run, report_lint_json,
    report_lint_json_run, report_lint_toon, report_lint_toon_run,
};
