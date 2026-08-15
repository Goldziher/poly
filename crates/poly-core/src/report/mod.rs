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
//! [`Verbosity`] selects how much of each diagnostic the `pretty` renderers
//! show:
//! - **default** — one terse line per finding (`level  engine  code?  line:col?
//!   title`). `description`, `url`, and `metadata` are hidden.
//! - **`--verbose`** — additionally renders `description`, `url`, and any
//!   `metadata` as indented lines, and lifts the cap on the skipped-file note.
//! - **`--debug`** — additionally renders a dim per-file debug block (engine
//!   version, cache hit/miss, timing).
//!
//! For `json` / `toon` the full structured record is **always** emitted (serde
//! omits empty/`None` fields), so `--verbose` is a no-op there; `--debug` simply
//! causes the runner to attach the `debug` field, which then serializes.
//!
//! The module is split by concern: `shared` holds the verbosity type and the
//! styling primitives, `notes` the discovery/skip qualification notes, `lint`
//! and `format` the human-oriented renderers for each run kind, and
//! `structured` the `json` / `toon` serialization.

mod format;
mod lint;
mod notes;
mod render;
mod shared;
mod structured;

pub use format::{
    eprint_format_errors, render_format_errors, render_format_pretty, render_format_pretty_run, report_format_pretty,
    report_format_pretty_run,
};
pub use lint::{
    eprint_lint_errors, render_lint_errors, render_lint_pretty, render_lint_pretty_run, report_lint_pretty,
    report_lint_pretty_run,
};
pub use notes::{eprint_discovery_note, eprint_skip_note, render_discovery_note, render_skip_note};
pub use render::RenderError;
pub use shared::Verbosity;
pub use structured::{
    report_format_json, report_format_json_run, report_format_toon, report_format_toon_run, report_lint_json,
    report_lint_json_run, report_lint_toon, report_lint_toon_run,
};
