//! `poly-hooks` — sync execution building blocks for the poly native hook runner.
//!
//! This crate is phase B0 of the hooks rewrite plan. It ports the low-level
//! execution primitives from the vendored `polyhooks` (prek) fork into a
//! synchronous, tokio-free form, ready for the rayon-based B1 runner.
//!
//! # Modules
//!
//! - [`consts`] — environment variable names (re-exported from `polyhooks`).
//! - [`identify`] — file-type tagging by filename/shebang (re-exported from `polyhooks`).
//! - [`stage`] — [`Stage`] enum + [`HookType`] and their mapping.
//! - [`process`] — synchronous [`Cmd`] wrapper over [`std::process::Command`].
//! - [`git`] — synchronous git helpers (staged files, diff, worktree state).
//! - [`install`] — install/uninstall the git-hook shims that invoke `poly hooks hook-impl`.
//! - [`hook_impl`] — parse a fired git hook's args/stdin into [`hook_impl::RunInputs`].
//! - [`filter`] — filename + tag-based file filtering primitives.
//! - [`reporter`] — output rendering helpers ([`reporter::OutputPreview`], status markers).
//! - [`fs`] — path utilities (clean, simplify, normalize, relative).
//! - [`cleanup`] — global cleanup hook registry.
//! - [`pty`] (Unix-only) — blocking PTY primitives for colored subprocess output.
//! - [`model`] — the in-memory hook model ([`Hook`], [`StageSpec`], request/outcome types).
//! - [`concurrency`] — rayon pool sizing + `ARG_MAX` file batching.
//! - [`runner`] — the native rayon hook runner ([`run`]).
//!
//! # Entry point
//!
//! [`run`] executes a [`HookRunRequest`] (a set of [`StageSpec`]s) on a
//! dedicated rayon pool and returns a [`HookRunOutcome`]. Per stage the order is
//! precondition → before → hooks (rayon) → after.

// Allow missing_docs on the re-exported modules since they originate in the
// vendored polyhooks crate, which is exempt from our doc requirements.
#![allow(missing_docs)]

/// Environment variable names. Re-exported from the vendored `polyhooks` crate.
/// These will be inlined when `polyhooks` is removed in B4.
pub mod consts {
    pub use polyhooks::consts::*;
}

/// File-type identification by filename, shebang, and interpreter.
/// Re-exported from the vendored `polyhooks` crate (already fully synchronous).
/// Will be inlined when `polyhooks` is removed in B4.
pub mod identify {
    pub use polyhooks::identify::*;
}

pub mod cleanup;
pub mod concurrency;
pub mod filter;
pub mod fs;
pub mod git;
pub mod hook_impl;
pub mod install;
pub mod model;
pub mod process;
pub mod reporter;
pub mod runner;
pub mod stage;

#[cfg(unix)]
pub mod pty;

// Re-export the most commonly used types at the crate root for convenience.
pub use hook_impl::{PushInfo, RunInputs};
pub use model::{
    Hook, HookCache, HookCommand, HookOutcome, HookRunOutcome, HookRunRequest, HookStatus,
    SccacheSettings, SkipReason, StageOutcome, StageSpec, StageStatus, StepOutcome,
};
pub use process::{Cmd, OutputSink};
pub use reporter::{CaptureSink, HookRunReporter};
pub use runner::run;
pub use stage::{HookType, Stage};
