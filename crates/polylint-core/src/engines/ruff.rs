//! Python backend: formatting via `ruff_python_formatter`, lint via
//! `ruff_python_parser` (parse-error diagnostics only).
//!
//! Capabilities: [`Capabilities::format`] + [`Capabilities::lint`].
//! These crates are consumed as published dependencies (astral-sh publishes
//! ruff's component crates to crates.io). Full rule-based lint (`ruff_linter`)
//! is deferred to a later milestone.
//!
//! # Opinionated defaults layered on top of ruff's own defaults
//!
//! | Setting | Polylint default | ruff default |
//! |---|---|---|
//! | `line-length` | 120 | 88 |
//! | `docstring-code-format` | `true` | `false` |
//! | `docstring-code-line-width` | 120 | dynamic |
//!
//! These defaults are overridden by any `[fmt.python.ruff]` or
//! `[lint.python.ruff]` table in the user's `polylint.toml`.

use ruff_formatter::LineWidth;
use ruff_python_formatter::{DocstringCode, DocstringCodeLineWidth, PyFormatOptions};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// Ruff Python backend.
pub struct RuffEngine;

static LANGUAGES: &[Language] = &[Language::Python];

impl Engine for RuffEngine {
    fn name(&self) -> &'static str {
        "ruff"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        // Tracks the published `ruff_python_formatter` crate version; bump in
        // lock-step with the dependency so cached output is invalidated when the
        // formatter changes.
        "0.0.3"
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // Only parse-error diagnostics for now.
        let result = parse_module(&src.content);
        match result {
            Ok(_) => Ok(Vec::new()),
            Err(err) => {
                let index = LineIndex::from_source_text(&src.content);
                let start = index.line_column(err.location.start(), &src.content);
                let end = index.line_column(err.location.end(), &src.content);
                Ok(vec![Diagnostic {
                    engine: "ruff".to_string(),
                    code: Some("E999".to_string()),
                    severity: Severity::Error,
                    message: err.error.to_string(),
                    span: Some(Span {
                        start_line: start.line.get() as u32,
                        start_col: start.column.get() as u32,
                        end_line: end.line.get() as u32,
                        end_col: end.column.get() as u32,
                    }),
                    fix: None,
                    metadata: Default::default(),
                }])
            }
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let line_width = u16::try_from(cfg.globals.line_length)
            .ok()
            .and_then(|w| LineWidth::try_from(w).ok())
            .unwrap_or_else(|| LineWidth::try_from(120_u16).unwrap());

        let options = PyFormatOptions::from_extension(&src.path)
            .with_line_width(line_width)
            .with_docstring_code(DocstringCode::Enabled)
            .with_docstring_code_line_width(DocstringCodeLineWidth::Fixed(line_width));

        match ruff_python_formatter::format_module_source(&src.content, options) {
            Ok(printed) => {
                let formatted = printed.into_code();
                if formatted == src.content {
                    Ok(FormatOutput::Unchanged)
                } else {
                    Ok(FormatOutput::Formatted(formatted))
                }
            }
            Err(err) => Err(anyhow::anyhow!("ruff format error: {err}")),
        }
    }
}
