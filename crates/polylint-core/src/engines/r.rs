//! R backend: formatting via [`air_r_formatter`], linting via [`jarl_core`].
//!
//! `air` is a pure-Rust R formatter backed by a Posit-forked biome CST engine.
//! `jarl` is a pure-Rust R linter that reuses the same air parser.  Both run
//! in-process — no subprocess, no system dependency.
//!
//! ## Capabilities
//! - **Format**: reformat `.R` files with opinionated overrides (line width 120,
//!   indent width from [`EngineConfig`], space indent).
//! - **Lint**: run jarl's default-enabled rule set against `.R` files.
//! - **Fix**: apply safe jarl autofixes (byte-range edits) produced by the lint
//!   rules that advertise a [`jarl_core::rule_set::FixStatus::Safe`] fix.
//!
//! ## Config layering
//! Format: air defaults → opinionated override (line_width 120, indent_style Space,
//! indent_width from `cfg.indent_width`) → user `[fmt.r.r]` table.
//!
//! Lint: jarl's default-enabled rules, no minimum R version assumed (conservative:
//! version-gated rules are excluded).
//!
//! ## Cache key
//! [`VERSION`] folds both the air and jarl git revs so any fork bump invalidates
//! stale cached output.

use std::collections::HashMap;
use std::sync::LazyLock;

use air_r_formatter::context::RFormatOptions;
use air_r_formatter::format_node;
use air_r_parser::RParserOptions;
use air_settings::{IndentStyle, IndentWidth, LineWidth};
use jarl_core::check::get_checks as jarl_get_checks;
use jarl_core::config::{ArgsConfig, Config, build_config};
use jarl_core::diagnostic::Diagnostic as JarlDiagnostic;
use jarl_core::package::{FilePackageInfo, PackageAnalysis, PackageContext};

use crate::config::EngineConfig;
use crate::engine::{
    Capabilities, Diagnostic, Edit, Engine, FormatOutput, Severity, SourceFile, Span,
};
use crate::language::Language;

/// Cache key version: folds both the air and jarl git revs so any fork bump
/// invalidates stale cached output.
///
/// - air rev: `c916545f14f76e1d6bd6ff918870f86dfa704b63` (first 7 chars)
/// - jarl rev: `24e39d0405e9a358ae988e5f8f86fa5437e3fdd9` (first 7 chars)
const VERSION: &str = "air:c916545 jarl:24e39d0";

/// Tier-1 languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::R];

/// jarl [`Config`] built once at first lint call and reused across the rayon
/// `par_iter`.  Building config is cheap (no filesystem access with empty paths),
/// but the resulting `Config` is `Send + Sync` so it can safely live in a
/// `LazyLock` and be shared across threads without additional locking.
///
/// Rule selection: default-enabled rules, no minimum R version specified
/// (conservative — version-gated rules such as `grepv` are excluded).
static JARL_CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let args = ArgsConfig {
        files: vec![],
        fix: false,
        unsafe_fixes: false,
        fix_only: false,
        // Empty select → `all_rules_enabled_by_default()` (excludes opt-in
        // categories like Dplyr that need a live R package cache to be useful).
        select: String::new(),
        extend_select: String::new(),
        ignore: String::new(),
        // No minimum version → version-gated rules are excluded (conservative).
        min_r_version: None,
        // polylint is not a VCS-aware fix tool; these flags only affect the fix
        // path which we never trigger from this engine.
        allow_dirty: true,
        allow_no_vcs: true,
        assignment: None,
    };
    // Empty paths → `determine_minimum_r_version` loops over nothing (no FS
    // access).  This is safe to call at init time.
    build_config(&args, None, vec![])
        .expect("jarl: failed to build default config — this is a bug in polylint")
});

/// Tier-1 R backend — formats and lints `.R` files using the `air` formatter
/// and `jarl` linter in-process.
pub struct REngine;

impl Engine for REngine {
    fn name(&self) -> &'static str {
        "r"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        VERSION
    }

    /// Lint `src.content` with jarl.
    ///
    /// Uses an in-process call to [`jarl_core::check::get_checks`] with empty
    /// cross-file package context — polylint is a per-file linter and does not
    /// perform multi-file package analysis.  Package-specific rules (unused
    /// function, duplicated definition) still run but produce no diagnostics
    /// without cross-file data, which is correct: false negatives are preferable
    /// to false positives in a general-purpose linter.
    ///
    /// Parse errors are silently swallowed (`Ok(vec![])`) so that a broken R
    /// file does not surface confusing "parse error" diagnostics.  The formatter
    /// already handles parse errors by returning `Unchanged`.
    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let pkg = PackageAnalysis::default();
        let pkg_contexts: HashMap<_, PackageContext> = HashMap::new();
        let file_pkg_info: HashMap<_, FilePackageInfo> = HashMap::new();

        match jarl_get_checks(
            &src.content,
            &src.path,
            &JARL_CONFIG,
            &pkg,
            &pkg_contexts,
            &file_pkg_info,
        ) {
            Ok(jarl_diags) => Ok(jarl_diags
                .into_iter()
                .map(|d| map_jarl_diagnostic(d, &src.content))
                .collect()),
            Err(e) if e.is::<jarl_core::error::ParseError>() => {
                // Corrupt/partial R — graceful degradation; the format path
                // returns Unchanged for the same input.
                Ok(vec![])
            }
            Err(e) => Err(e),
        }
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

// ---------------------------------------------------------------------------
// Diagnostic mapping helpers
// ---------------------------------------------------------------------------

/// Convert a byte offset into a 1-based (line, column) pair using the source
/// content.  Clamps the offset to the content length so out-of-bounds offsets
/// do not panic.
fn byte_to_span_pos(content: &str, byte_offset: usize) -> (u32, u32) {
    let safe = byte_offset.min(content.len());
    let before = &content[..safe];
    let line = before.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let col_start = before.rfind('\n').map_or(0, |p| p + 1);
    // Column is the number of bytes from the last newline (or SOF) to the offset,
    // plus 1 for 1-based indexing.
    let col = (safe - col_start) as u32 + 1;
    (line, col)
}

/// Map a [`JarlDiagnostic`] to a polylint [`Diagnostic`].
///
/// Severity is always [`Severity::Warning`] — jarl violations have no severity
/// field; they are all style/correctness warnings, never fatal errors.
///
/// A fix edit is included when the jarl fix is not marked `to_skip` and has
/// non-empty replacement content.  `to_skip` is a jarl-internal flag indicating
/// that the autofix for a particular node is temporarily disabled (e.g., because
/// the node contains a comment that would be misplaced after the edit).
fn map_jarl_diagnostic(jarl_diag: JarlDiagnostic, content: &str) -> Diagnostic {
    let start_byte: usize = jarl_diag.range.start().into();
    let end_byte: usize = jarl_diag.range.end().into();
    let (start_line, start_col) = byte_to_span_pos(content, start_byte);
    let (end_line, end_col) = byte_to_span_pos(content, end_byte);

    let fix = if !jarl_diag.fix.to_skip && !jarl_diag.fix.content.is_empty() {
        vec![Edit {
            start_byte: jarl_diag.fix.start,
            end_byte: jarl_diag.fix.end,
            replacement: jarl_diag.fix.content,
        }]
    } else {
        vec![]
    };

    Diagnostic {
        engine: "r".to_string(),
        code: Some(jarl_diag.message.name),
        severity: Severity::Warning,
        title: jarl_diag.message.body,
        description: None,
        url: None,
        span: Some(Span {
            start_line,
            start_col,
            end_line,
            end_col,
        }),
        fix,
        metadata: Default::default(),
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
        assert!(caps.lint, "lint capability must be true");
        assert!(caps.format, "format capability must be true");
        assert!(caps.fix, "fix capability must be true");
        assert_eq!(engine.version(), VERSION);
    }

    #[test]
    fn lint_clean_r_has_no_diagnostics() {
        // Clean, idiomatic R — should trigger zero jarl rules.
        let engine = REngine;
        let src = make_src("x <- 1\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "expected no diagnostics for clean R, got: {diags:?}"
        );
    }

    #[test]
    fn lint_parse_error_returns_empty() {
        // A bare `function(` never closes — jarl returns ParseError.
        // polylint must swallow it and return Ok(vec![]).
        let engine = REngine;
        let src = make_src("function(\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(
            diags.is_empty(),
            "parse error must degrade to empty diagnostics"
        );
    }

    #[test]
    fn lint_equals_na_produces_diagnostic_with_fix() {
        // `x == NA` is flagged by the `equals_na` rule (default-enabled, safe fix).
        let engine = REngine;
        let src = make_src("x <- c(1, 2, NA)\ny <- x == NA\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();

        let equals_na: Vec<_> = diags
            .iter()
            .filter(|d| d.code.as_deref() == Some("equals_na"))
            .collect();
        assert!(
            !equals_na.is_empty(),
            "expected at least one equals_na diagnostic, got: {diags:?}"
        );

        let d = &equals_na[0];
        assert_eq!(d.engine, "r");
        assert_eq!(d.severity, Severity::Warning);
        // The fix replaces `x == NA` with `is.na(x)`.
        assert!(!d.fix.is_empty(), "equals_na must include an autofix Edit");
        assert!(d.span.is_some(), "equals_na must include a source Span");

        let span = d.span.unwrap();
        // The diagnostic is on the second line (y <- x == NA), so start_line >= 2.
        assert!(
            span.start_line >= 2,
            "equals_na span must point to the second line, got line {}",
            span.start_line
        );
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
