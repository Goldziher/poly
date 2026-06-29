//! Core engine for **polylint** (lint) and **polyfmt** (format): a universal,
//! zero-dependency linter/formatter that wraps best-in-class tools as in-process
//! Rust backends behind a single [`Engine`] trait.
//!
//! Architecture (see the project plan): files are discovered, routed to backends
//! via the registry, run in parallel ([`runner`], rayon), cached by content hash
//! (blake3), and reported ([`report`]).
//!
//! New backends implement [`engine::Engine`] and are wired into the registry.
//! The tree-sitter generic tier serves any language without a native backend.
//!
//! Result caching is provided by the shared `poly-cache` crate. The `engines`
//! and `discover` modules are `#[doc(hidden)]`: they are reachable for the
//! in-crate integration tests but are not part of the stable public API.
//! `registry` is crate-private. Downstream consumers use the curated re-exports
//! below plus [`lint`] / [`format()`].

// Public-for-tests, not part of the stable API: the per-backend integration
// tests under `tests/` construct engines and exercise discovery directly, so
// these stay `pub` but are hidden from the documented surface.
pub mod config;
pub mod defaults;
#[doc(hidden)]
pub mod discover;
pub mod engine;
#[doc(hidden)]
pub mod engines;
pub mod language;
pub(crate) mod registry;
pub mod report;
pub mod runner;

pub use config::{Config, Kind};
pub use engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
pub use language::Language;
pub use report::Verbosity;
pub use runner::{EngineDebug, FormatResult, LintResult, RunDebug, RunOptions, format, lint};
