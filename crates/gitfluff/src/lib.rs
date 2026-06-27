//! gitfluff — commit-message linting, cleanup, and git-hook installation.
//!
//! This crate is both a standalone binary (`gitfluff`) and a library so the
//! polylint workspace can drive the same logic in-process from a future
//! `poly commit` subcommand.
//!
//! The reusable core is [`lint::lint_message`], a pure function over a
//! [`lint::LintOptions`] that returns a [`lint::LintOutcome`]; [`config`] and
//! [`presets`] load and resolve configuration, and the private `app` module
//! wires it all into the CLI flow via [`run`] / [`run_lint`].

// gitfluff is vendored from the user's standalone MIT crate with its full,
// broad public surface intact (so the standalone binary keeps building). That
// surface will be narrowed and documented when the `poly commit` integration
// curates the in-process API (task #34); until then, exempt this one crate
// from the workspace `-D missing-docs` rustdoc gate. Broken intra-doc links
// are still denied — only missing docs are allowed here.
#![allow(missing_docs)]

pub mod cli;
pub mod config;
pub mod hooks;
pub mod lint;
pub mod presets;

mod app;

pub use app::{main_entry, run, run_hook_install, run_lint};
