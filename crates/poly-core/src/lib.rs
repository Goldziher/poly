//! Core engine for `poly`: a universal, zero-dependency linter/formatter that
//! wraps best-in-class tools as in-process Rust backends behind a single
//! [`Engine`] trait.
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
//!
//! [`lint`] and [`format()`] return only the per-file results and **drop** the
//! per-file failures the run recorded, so a file an engine failed on is simply
//! absent from what they return — indistinguishable from one that was checked
//! and found clean. Any caller that gates on the outcome must use [`lint_run`] /
//! [`format_run`] and inspect [`LintRun::errors`] / [`FormatRun::errors`].

pub mod config;
pub mod defaults;
#[doc(hidden)]
pub mod discover;
pub mod engine;
#[doc(hidden)]
pub mod engines;
pub(crate) mod filter;
pub mod language;
pub(crate) mod registry;
pub mod report;
#[doc(hidden)]
pub mod resolve;
pub mod runner;

pub use config::{Config, Kind};
pub use discover::{DiscoveryReport, ExcludedRule};
pub use engine::{Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span};
pub use language::Language;
pub use report::Verbosity;
pub use resolve::{ConfigSet, ExcludeRule};
pub use runner::{
    EngineDebug, FormatError, FormatResult, FormatRun, LintError, LintResult, LintRun, NO_ENGINE_SKIP,
    NO_LINT_RULES_SKIP_PREFIX, RunDebug, RunOptions, SkippedFile, format, format_run, lint, lint_run,
};
