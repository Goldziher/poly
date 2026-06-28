//! R backend: formatting via [`air_r_formatter`].
//!
//! `air` is a pure-Rust R formatter backed by a Posit-forked biome CST engine.
//! This backend wraps `air_r_parser` + `air_r_formatter` in-process — no subprocess,
//! no system dependency.
//!
//! ## Capabilities
//! - **Format**: reformat `.R` files with opinionated overrides (line width 120,
//!   indent width from [`EngineConfig`], space indent).
//! - **Lint**: stub returning `Ok(vec![])` — wired to `jarl_core` once pushed.
//!   See TODO below.
//!
//! ## Config layering
//! air defaults → opinionated override (line_width 120, indent_style Space,
//! indent_width from `cfg.indent_width`) → user `[fmt.r.r]` table.
//!
//! ## Cache key
//! [`VERSION`] folds the air git rev so any fork bump invalidates cached output.

use air_r_formatter::context::RFormatOptions;
use air_r_formatter::format_node;
use air_r_parser::RParserOptions;
use air_settings::{IndentStyle, IndentWidth, LineWidth};

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Engine, FormatOutput, SourceFile};
use crate::language::Language;

/// Cache key version: folds the air git rev so any fork bump invalidates stale output.
///
/// Format: `"air:<short-rev>"` where the short rev is the first 7 hex chars of the
/// pinned commit `c916545f14f76e1d6bd6ff918870f86dfa704b63`.
const VERSION: &str = "air:c916545";

/// Tier-1 languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::R];

/// Tier-1 R backend — formats `.R` files using the `air` in-process R formatter.
pub struct REngine;

impl Engine for REngine {
    fn name(&self) -> &'static str {
        "r"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    /// Format-only for now; lint is wired once `jarl_core` is available.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: false,
            format: true,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// Lint stub — always returns no diagnostics.
    ///
    // TODO(#31): wire jarl_core::check::get_checks once the jarl fork is pushed.
    fn lint(&self, _src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        Ok(vec![])
    }

    /// Format `src.content` with air. Returns [`FormatOutput::Unchanged`] when:
    /// - the formatter output equals the input (file is already well-formatted), or
    /// - the file has parse errors (corrupt/partial R is left untouched).
    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let parse = air_r_parser::parse(&src.content, RParserOptions::default());

        // Parse error → leave the file untouched rather than risk data loss.
        if parse.has_error() {
            return Ok(FormatOutput::Unchanged);
        }

        // Build options from the resolved EngineConfig.
        // line_length is usize; LineWidth accepts u16 in the range 1..=320.
        // If the value is out of range, fall back to air's default (80) which is
        // overridden to 120 by the GlobalDefaults.
        let line_width = u16::try_from(cfg.globals.line_length)
            .ok()
            .and_then(|w| LineWidth::try_from(w).ok())
            .unwrap_or_default();
        // indent_width is usize; IndentWidth accepts values 1..=24.
        let indent_width = IndentWidth::try_from(cfg.indent_width).unwrap_or_default();

        let opts = RFormatOptions::new()
            .with_line_width(line_width)
            .with_indent_style(IndentStyle::Space)
            .with_indent_width(indent_width);

        let code = format_node(opts, &parse.syntax())
            .map_err(|e| anyhow::anyhow!("air: format_node failed: {e}"))?
            .print()
            .map_err(|e| anyhow::anyhow!("air: print failed: {e}"))?
            .into_code();

        if code == src.content.as_ref() {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(code))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.R"),
            language: Language::R,
            content: content.into(),
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn engine_metadata() {
        let engine = REngine;
        assert_eq!(engine.name(), "r");
        assert_eq!(engine.languages(), &[Language::R]);
        let caps = engine.capabilities();
        assert!(!caps.lint);
        assert!(caps.format);
        assert!(!caps.fix);
        assert_eq!(engine.version(), VERSION);
    }

    #[test]
    fn lint_always_returns_empty() {
        let engine = REngine;
        let src = make_src("x <- 1\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn unformatted_input_returns_formatted() {
        let engine = REngine;
        let src = make_src("x<-1+2\nf<-function(a,b){a+b}\n");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Formatted(_)),
            "expected Formatted for unformatted input"
        );
    }

    #[test]
    fn already_formatted_input_is_unchanged() {
        let engine = REngine;
        // This is the canonical air-formatted output for the unformatted fixture.
        let formatted = "x <- 1 + 2\nf <- function(a, b) {\n  a + b\n}\n";
        let src = make_src(formatted);
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Unchanged),
            "expected Unchanged for already-formatted input"
        );
    }

    #[test]
    fn unparsable_input_is_unchanged() {
        let engine = REngine;
        // A bare `function(` never closes; air should report a parse error.
        let src = make_src("function(\n");
        let out = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(out, FormatOutput::Unchanged),
            "expected Unchanged for unparsable input"
        );
    }
}
