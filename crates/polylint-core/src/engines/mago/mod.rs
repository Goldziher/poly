//! PHP backend: lint + format via the `mago` toolchain (carthage-software/mago,
//! MIT OR Apache-2.0).
//!
//! # Lint (`lint.rs`)
//! Parses the file with `mago-syntax`, resolves names with `mago-names`, then
//! runs every enabled `mago-linter` rule. Parse errors in `program.errors` are
//! surfaced first (code `"syntax"` / `"parse"`, [`crate::engine::Severity`]
//! `::Error`), followed by the linter's `mago_reporting::IssueCollection`
//! mapped to polylint [`Diagnostic`]s. Where a lint issue carries exactly one
//! safe `mago_text_edit::TextEdit`, a [`crate::engine::Edit`] fix is wired so
//! `--fix` can apply it.
//!
//! # Format (`format.rs`)
//! Delegates to [`mago_formatter::Formatter`] with settings derived from
//! [`EngineConfig`]: `line_length → print_width`, `indent_width → tab_width`.
//! Returns [`FormatOutput::Unchanged`] when the formatted output equals the
//! input or when parsing fails (the lint pass surfaces the parse error instead).
//!
//! # PHP version
//! Defaults to PHP 8.4, the latest stable version at the time of writing.
//!
//! # Capabilities
//! Lint, format, and single-edit safe autofixes.

mod format;
mod lint;
pub(super) mod rules;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// PHP backend using the `mago` linter + formatter.
///
/// The engine is a zero-sized unit struct; all state (arena, parsed program,
/// linter registry) is created per-call so the engine is `Send + Sync` and
/// can run inside a rayon `par_iter`.
pub struct MagoEngine;

static LANGUAGES: &[Language] = &[Language::Php];

impl Engine for MagoEngine {
    fn name(&self) -> &'static str {
        "mago"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            // Single-edit safe fixes are wired in lint.rs.
            fix: true,
        }
    }

    /// Cache-key version: bump whenever mago output could change.
    fn version(&self) -> &str {
        "mago-1.42.0"
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        lint::lint_php(src, cfg)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        format::format_php(src, cfg)
    }
}
