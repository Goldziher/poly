//! Core engine for **polylint** (lint) and **polyfmt** (format): a universal,
//! zero-dependency linter/formatter that wraps best-in-class tools as in-process
//! Rust backends behind a single [`Engine`] trait.
//!
//! Architecture (see the project plan): files are discovered ([`discover`]),
//! routed to backends via the [`registry`], run in parallel ([`runner`], rayon),
//! cached by content hash ([`cache`], blake3), and reported ([`report`]).
//!
//! New backends implement [`engine::Engine`] and are wired into
//! [`registry::engines_for`]. [`engines::treesitter::TreeSitterEngine`] is the
//! generic tier that serves any language without a native backend.

pub mod cache;
pub mod config;
pub mod defaults;
pub mod discover;
pub mod engine;
pub mod engines;
pub mod language;
pub mod registry;
pub mod report;
pub mod runner;

pub use config::{Config, Kind};
pub use engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
pub use language::Language;
pub use runner::{FormatResult, LintResult, RunOptions, format, lint};
