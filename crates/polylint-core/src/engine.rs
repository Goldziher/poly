//! The [`Engine`] trait every backend implements, plus the normalized diagnostic
//! and format-output types backends produce.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::EngineConfig;
use crate::language::Language;

/// A single file to be linted or formatted.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Path to the file on disk.
    pub path: PathBuf,
    /// Detected language of the file.
    pub language: Language,
    /// Full file contents. Held as `Arc<str>` so a single file's bytes can be
    /// shared across every engine that runs on it (and across fix passes)
    /// without re-cloning the contents on the per-file hot path.
    pub content: Arc<str>,
}

/// What a backend is able to do for its language(s).
#[derive(Debug, Clone, Copy, Default)]
pub struct Capabilities {
    /// The backend can report diagnostics.
    pub lint: bool,
    /// The backend can reformat source.
    pub format: bool,
    /// The backend can produce autofixes for its diagnostics.
    pub fix: bool,
}

/// Severity of a [`Diagnostic`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A problem that should fail the run.
    Error,
    /// A likely problem that does not fail the run by default.
    Warning,
    /// Informational note.
    Info,
    /// A low-priority suggestion.
    Hint,
}

/// 1-based line/column source span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    /// 1-based line of the span start.
    pub start_line: u32,
    /// 1-based column of the span start.
    pub start_col: u32,
    /// 1-based line of the span end.
    pub end_line: u32,
    /// 1-based column of the span end.
    pub end_col: u32,
}

/// A byte-range replacement used to apply an autofix.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edit {
    /// Inclusive start byte offset into the source.
    pub start_byte: usize,
    /// Exclusive end byte offset into the source.
    pub end_byte: usize,
    /// Text to substitute for the byte range.
    pub replacement: String,
}

/// A normalized lint finding, uniform across all backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Id of the backend that produced this finding.
    pub engine: String,
    /// Tool-specific rule code, if any.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    /// Severity of the finding.
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
    /// Source location, if known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub span: Option<Span>,
    /// Suggested autofixes, if available.  A non-empty Vec is applied
    /// atomically: either all edits apply, or none do (see `runner::apply_edits`).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub fix: Vec<Edit>,
    /// Tool-specific extras (rule URL, fix applicability, category, …), rendered
    /// verbatim by the output layer. Empty for most findings.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Result of a format pass.
#[derive(Debug, Clone)]
pub enum FormatOutput {
    /// The input was already formatted.
    Unchanged,
    /// The formatted source (differs from the input).
    Formatted(String),
}

/// A linter/formatter backend. Backends are pure functions of
/// `(source, config)` so results can be content-hash cached.
///
/// # Stability
///
/// This trait is an **internal extension point**, not part of the stable public
/// API. Backends are implemented within this crate and reached through the
/// [`lint`](crate::lint) / [`format`](crate::format) orchestrators; implementing
/// it downstream is unsupported and may break without notice.
pub trait Engine: Send + Sync {
    /// Stable backend id (e.g. `"taplo"`, `"oxc"`), used in config + cache keys.
    fn name(&self) -> &'static str;

    /// Tier-1 languages this backend explicitly handles. The generic tier may
    /// return an empty slice and rely on registry routing.
    fn languages(&self) -> &'static [Language];

    /// What this backend can do (lint/format/fix).
    fn capabilities(&self) -> Capabilities;

    /// Version of the wrapped tool/crate; folded into the cache key so a tool
    /// upgrade invalidates stale cached results.
    fn version(&self) -> &str;

    /// Lint a file, returning normalized diagnostics. Defaults to no findings.
    fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        Ok(Vec::new())
    }

    /// Format a file. Defaults to [`FormatOutput::Unchanged`].
    fn format(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        Ok(FormatOutput::Unchanged)
    }
}
