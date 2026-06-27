//! YAML backend: validity lint via `saphyr`, structural format via `pretty_yaml`.
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
//! Delegates to [`pretty_yaml::format_text`] which performs a full CST-based
//! structural reflow: normalized indentation, colon spacing, quote
//! canonicalization, trailing whitespace removal, and final-newline
//! enforcement. Config is mapped: `line_length → print_width`,
//! `indent_width → indent_width`, `line_ending → line_break`.
//! Returns [`FormatOutput::Unchanged`] when the output equals the input.

use pretty_yaml::config::{FormatOptions, LanguageOptions, LayoutOptions, LineBreak};
use saphyr::{LoadableYamlNode, Yaml};

use crate::config::{EngineConfig, LineEnding};
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// YAML backend (validity lint + structural format via `pretty_yaml`).
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
        // Tracks the saphyr version (lint) and pretty_yaml version (format).
        // Bump when either dependency version changes so stale cached output is
        // invalidated — both are folded into the cache key.
        "0.0.6+pretty_yaml-0.6.0"
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        // Config is intentionally unused: saphyr validity is binary (valid YAML
        // or a syntax error). There are no rule toggles, thresholds, or
        // dialect settings available through the saphyr API.
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
                    metadata: Default::default(),
                }])
            }
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let line_break = match cfg.globals.line_ending {
            LineEnding::Crlf => LineBreak::Crlf,
            LineEnding::Lf => LineBreak::Lf,
        };
        let options = FormatOptions {
            layout: LayoutOptions {
                print_width: cfg.globals.line_length,
                indent_width: cfg.indent_width,
                line_break,
            },
            language: LanguageOptions::default(),
        };
        // Unparsable YAML: leave the file untouched rather than failing the
        // whole run. The saphyr-based `lint` path already surfaces the syntax
        // error as a diagnostic, so we don't lose the signal.
        let formatted = match pretty_yaml::format_text(&src.content, &options) {
            Ok(formatted) => formatted,
            Err(_) => return Ok(FormatOutput::Unchanged),
        };
        if formatted == *src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}
