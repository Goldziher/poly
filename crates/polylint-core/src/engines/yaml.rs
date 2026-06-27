//! YAML backend: validity lint via `saphyr`, whitespace-safe format.
//!
//! Capabilities: [`Capabilities::lint`] + [`Capabilities::format`]. No fix.
//!
//! # Lint
//! Runs the source through [`saphyr::Yaml::load_from_str`]. Any
//! [`saphyr::ScanError`] becomes a single [`Diagnostic`] with code
//! `"syntax"` and [`Severity::Error`], carrying the 1-based line/column from
//! the scanner's [`saphyr::Marker`].
//!
//! # Format
//! Structural YAML reflow is explicitly out of scope: comment preservation
//! across a round-trip is unsolved and would silently corrupt files. Format
//! is therefore limited to [`crate::defaults::normalize_whitespace`]:
//! trim trailing whitespace, normalize line endings, enforce a final newline.
//! Returns [`FormatOutput::Unchanged`] when the result equals the input.

use saphyr::{LoadableYamlNode, Yaml};

use crate::config::EngineConfig;
use crate::defaults::normalize_whitespace;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// YAML backend (validity lint + whitespace-safe format).
pub struct YamlEngine;

static LANGUAGES: &[Language] = &[Language::Yaml];

impl Engine for YamlEngine {
    fn name(&self) -> &'static str {
        "yaml"
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
        // Tracks the published `saphyr` crate version; bump when the
        // dependency version changes so stale cached output is invalidated.
        "0.0.6"
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match Yaml::load_from_str(&src.content) {
            Ok(_) => Ok(Vec::new()),
            Err(err) => {
                let marker = err.marker();
                // Marker::line() is 1-based; Marker::col() is 0-based — convert
                // to the 1-based Span convention used by all polylint backends.
                let line = marker.line() as u32;
                let col = (marker.col() + 1) as u32;
                Ok(vec![Diagnostic {
                    engine: "yaml".to_string(),
                    code: Some("syntax".to_string()),
                    severity: Severity::Error,
                    message: err.to_string(),
                    span: Some(Span {
                        start_line: line,
                        start_col: col,
                        end_line: line,
                        end_col: col,
                    }),
                    fix: None,
                }])
            }
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let normalized = normalize_whitespace(&src.content, &cfg.globals);
        if normalized == src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(normalized))
        }
    }
}
