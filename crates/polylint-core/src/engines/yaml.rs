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
//! enforcement.
//!
//! ## Options layering
//! pretty_yaml defaults → polylint opinionated override (print_width=120,
//! indent_width from language default, line_break from global line_ending) →
//! user `[fmt.yaml.yaml]` table.  The user table is deserialized into
//! [`pretty_yaml::config::FormatOptions`] (via the `config_serde` feature)
//! then poly's layout fields are applied on top, so `print_width` and
//! `indent_width` always come from poly globals regardless of what the user
//! writes in the options table.  All [`pretty_yaml::config::LanguageOptions`]
//! fields are user-controllable.
//!
//! Returns [`FormatOutput::Unchanged`] when the output equals the input.

use pretty_yaml::config::{FormatOptions, LineBreak};
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
        // Bumped from 0.0.6 to 0.0.7 to invalidate caches after exposing full
        // LanguageOptions via config_serde (options were previously ignored).
        "0.0.7+pretty_yaml-0.6.0"
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
                    title: err.to_string(),
                    description: None,
                    url: None,
                    span: Some(Span {
                        start_line: line,
                        start_col: col,
                        end_line: line,
                        end_col: col,
                    }),
                    fix: vec![],
                    metadata: Default::default(),
                }])
            }
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let options = build_format_options(cfg);
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

// ── Options construction ──────────────────────────────────────────────────────

/// Build [`FormatOptions`] by layering user options over polylint opinionated
/// defaults over pretty_yaml's own defaults.
///
/// 1. Start with `FormatOptions::default()` (pretty_yaml's defaults).
/// 2. If `cfg.options` is non-empty, deserialise it into `FormatOptions` via
///    `config_serde`; unknown keys are silently ignored.
/// 3. Override layout fields with poly's globals (print_width, indent_width,
///    line_break) — these always come from poly, never from the user table.
fn build_format_options(cfg: &EngineConfig) -> FormatOptions {
    let mut options: FormatOptions = if cfg.options.is_empty() {
        FormatOptions::default()
    } else {
        toml::Value::Table(cfg.options.clone())
            .try_into()
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "[fmt.yaml.yaml] options could not be parsed; using defaults");
                FormatOptions::default()
            })
    };

    // Poly's layout overrides always win — they come from globals, not from
    // the per-engine options table.
    options.layout.print_width = cfg.globals.line_length;
    options.layout.indent_width = cfg.indent_width;
    options.layout.line_break = match cfg.globals.line_ending {
        LineEnding::Crlf => LineBreak::Crlf,
        LineEnding::Lf => LineBreak::Lf,
    };
    options
}
