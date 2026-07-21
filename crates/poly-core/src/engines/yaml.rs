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
//! pretty_yaml defaults → poly opinionated override (print_width=120,
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

use super::template::contains_go_template;
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
        "0.0.8+pretty_yaml-0.6.0+tmplskip"
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if contains_go_template(&src.content) {
            tracing::info!(path = %src.path.display(), "skipping file with Go/Helm template syntax");
            return Ok(Vec::new());
        }
        match Yaml::load_from_str(&src.content) {
            Ok(_) => Ok(Vec::new()),
            Err(err) => {
                let marker = err.marker();
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
        if contains_go_template(&src.content) {
            tracing::info!(path = %src.path.display(), "skipping file with Go/Helm template syntax");
            return Ok(FormatOutput::Unchanged);
        }
        let options = build_format_options(cfg);
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

/// Build [`FormatOptions`] by layering user options over poly opinionated
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

    options.layout.print_width = cfg.globals.line_length;
    options.layout.indent_width = cfg.indent_width;
    options.layout.line_break = match cfg.globals.line_ending {
        LineEnding::Crlf => LineBreak::Crlf,
        LineEnding::Lf => LineBreak::Lf,
    };
    options
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{EngineConfig, GlobalDefaults};

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    fn source(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("templates/deployment.yaml"),
            language: Language::Yaml,
            content: content.into(),
        }
    }

    #[test]
    fn helm_templated_yaml_is_skipped_not_errored() {
        let engine = YamlEngine;
        // Go-templated YAML is not valid YAML; without the skip saphyr reports a syntax error.
        let src =
            source("metadata:\n  name: {{ .Release.Name }}\n  {{- if .Values.labels }}\n  labels: {}\n  {{- end }}\n");
        assert!(
            engine.lint(&src, &default_cfg()).expect("lint succeeded").is_empty(),
            "templated YAML must be skipped, not reported as a syntax error"
        );
        assert!(
            matches!(
                engine.format(&src, &default_cfg()).expect("format succeeded"),
                FormatOutput::Unchanged
            ),
            "templated YAML must be left unchanged by format"
        );
    }

    #[test]
    fn github_actions_expression_is_still_linted() {
        let engine = YamlEngine;
        // `${{ }}` is a valid YAML scalar and must NOT be skipped.
        let src = source("on: push\njobs:\n  build:\n    if: ${{ github.event_name == 'push' }}\n");
        assert!(
            engine.lint(&src, &default_cfg()).expect("lint succeeded").is_empty(),
            "valid GitHub Actions YAML should lint clean (and not be skipped as a template)"
        );
    }

    #[test]
    fn invalid_yaml_still_reports_syntax_error() {
        let engine = YamlEngine;
        let src = source("foo: [1, 2\nbar: :\n");
        let diags = engine.lint(&src, &default_cfg()).expect("lint ran");
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("syntax")),
            "genuinely invalid YAML must still report a syntax error"
        );
    }
}
