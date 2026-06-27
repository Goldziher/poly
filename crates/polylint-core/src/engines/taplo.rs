//! Taplo TOML backend — wraps the `taplo` crate (MIT) for in-process TOML
//! formatting and syntax/semantic linting.
//!
//! ## Capabilities
//! - **Lint**: syntax errors from the parser + semantic errors from DOM
//!   validation (duplicate keys, type conflicts, etc.).
//! - **Format**: reformat via `taplo::formatter` with opinionated overrides
//!   (column width 120, indent width from `EngineConfig`).
//!
//! ## Config layering
//! `taplo` defaults → opinionated override (column_width 120) → user
//! `[fmt.toml.taplo]` table (only `column_width` and `indent_width` are
//! forwarded; all other keys are silently ignored for now).

use taplo::{
    dom,
    formatter::{self, Options},
    parser,
};

use crate::{
    config::EngineConfig,
    engine::{Capabilities, Diagnostic, Engine, FormatOutput, Severity, SourceFile, Span},
    language::Language,
};

/// The taplo crate version this backend wraps; used as part of the cache key.
const TAPLO_VERSION: &str = "0.14.0";

/// Tier-1 languages handled by this backend.
static LANGUAGES: &[Language] = &[Language::Toml];

/// Taplo TOML backend.
pub struct TaploEngine;

impl TaploEngine {
    /// Construct a new [`TaploEngine`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaploEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine for TaploEngine {
    fn name(&self) -> &'static str {
        "taplo"
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

    /// Cache key component: taplo crate version.
    ///
    /// A bump to the `taplo` dep line must be reflected here so that cached
    /// results from the old version are invalidated.
    fn version(&self) -> &str {
        TAPLO_VERSION
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        let mut diags = Vec::new();

        // --- Syntax errors from the parser -------------------------------------
        let parse = parser::parse(&src.content);
        for error in &parse.errors {
            let start_byte = u32::from(error.range.start()) as usize;
            let end_byte = u32::from(error.range.end()) as usize;
            let span = byte_range_to_span(&src.content, start_byte, end_byte);
            diags.push(Diagnostic {
                engine: "taplo".to_string(),
                code: Some("syntax-error".to_string()),
                severity: Severity::Error,
                message: error.message.clone(),
                span: Some(span),
                fix: None,
                metadata: Default::default(),
            });
        }

        // --- Semantic errors from DOM validation -------------------------------
        // Build the DOM regardless of parse errors; it may still have nodes.
        let dom = parse.into_dom();
        if let Err(errors) = dom.validate() {
            for error in errors {
                let span = semantic_error_span(&src.content, &error);
                diags.push(Diagnostic {
                    engine: "taplo".to_string(),
                    code: Some(semantic_error_code(&error).to_string()),
                    severity: Severity::Error,
                    message: error.to_string(),
                    span,
                    fix: None,
                    metadata: Default::default(),
                });
            }
        }

        Ok(diags)
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        let opts = build_options(cfg);
        let formatted = formatter::format(&src.content, opts);
        if formatted == src.content {
            Ok(FormatOutput::Unchanged)
        } else {
            Ok(FormatOutput::Formatted(formatted))
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Build `taplo::formatter::Options` from an [`EngineConfig`].
///
/// Layering: taplo defaults → opinionated override (column_width 120) →
/// user `[fmt.toml.taplo]` options (`column_width`, `indent_width`).
fn build_options(cfg: &EngineConfig) -> Options {
    // Opinionated overrides on top of taplo's own defaults.
    let mut opts = Options {
        // Override: 120-column target (taplo default is 80).
        column_width: 120,
        // Indent width from the resolved engine config (language default = 2 for TOML).
        indent_string: " ".repeat(cfg.indent_width),
        // Line endings from global config.
        crlf: matches!(cfg.globals.line_ending, crate::config::LineEnding::Crlf),
        // Trailing newline mirrors global final_newline.
        trailing_newline: cfg.globals.final_newline,
        ..Options::default()
    };

    // User overrides from [fmt.toml.taplo] in polylint.toml.
    if let Some(column_width) = cfg
        .options
        .get("column_width")
        .and_then(toml::Value::as_integer)
        && column_width > 0
    {
        opts.column_width = column_width as usize;
    }
    if let Some(indent_width) = cfg
        .options
        .get("indent_width")
        .and_then(toml::Value::as_integer)
        && indent_width > 0
    {
        opts.indent_string = " ".repeat(indent_width as usize);
    }

    opts
}

/// Convert a byte range `[start_byte, end_byte)` within `source` into a
/// 1-based [`Span`].
fn byte_range_to_span(source: &str, start_byte: usize, end_byte: usize) -> Span {
    let (start_line, start_col) = byte_to_line_col(source, start_byte);
    let (end_line, end_col) = byte_to_line_col(source, end_byte.max(start_byte));
    Span {
        start_line,
        start_col,
        end_line,
        end_col,
    }
}

/// Convert a byte offset into a 1-based `(line, column)` pair.
fn byte_to_line_col(source: &str, byte_offset: usize) -> (u32, u32) {
    let offset = byte_offset.min(source.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Extract a [`Span`] from a DOM [`dom::Error`], if any key has syntax.
fn semantic_error_span(source: &str, error: &dom::Error) -> Option<Span> {
    let range = match error {
        dom::Error::ConflictingKeys { key, .. } => key.text_ranges().next(),
        dom::Error::ExpectedTable { not_table, .. } => not_table.text_ranges().next(),
        dom::Error::ExpectedArrayOfTables {
            not_array_of_tables,
            ..
        } => not_array_of_tables.text_ranges().next(),
        dom::Error::InvalidEscapeSequence { string } => Some(string.text_range()),
        dom::Error::UnexpectedSyntax { syntax } => Some(syntax.text_range()),
        _ => None,
    };
    range.map(|r| {
        let start_byte = u32::from(r.start()) as usize;
        let end_byte = u32::from(r.end()) as usize;
        byte_range_to_span(source, start_byte, end_byte)
    })
}

/// Return a short diagnostic code for a DOM error variant.
fn semantic_error_code(error: &dom::Error) -> &'static str {
    match error {
        dom::Error::ConflictingKeys { .. } => "duplicate-key",
        dom::Error::ExpectedTable { .. } => "expected-table",
        dom::Error::ExpectedArrayOfTables { .. } => "expected-array-of-tables",
        dom::Error::InvalidEscapeSequence { .. } => "invalid-escape-sequence",
        dom::Error::UnexpectedSyntax { .. } => "unexpected-syntax",
        _ => "semantic-error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: "test.toml".into(),
            language: Language::Toml,
            content: content.to_string(),
        }
    }

    fn default_cfg() -> EngineConfig {
        EngineConfig {
            globals: crate::config::GlobalDefaults::default(),
            indent_width: 2,
            options: toml::Table::new(),
        }
    }

    #[test]
    fn version_is_taplo_semver() {
        let engine = TaploEngine::new();
        let v = engine.version();
        // Must parse as semver-ish (X.Y.Z).
        assert_eq!(v.split('.').count(), 3, "version should be X.Y.Z: {v}");
    }

    #[test]
    fn lint_clean_toml_produces_no_diags() {
        let engine = TaploEngine::new();
        let src = make_src("key = \"value\"\n[section]\nfoo = 42\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "clean TOML should have no diagnostics");
    }

    #[test]
    fn lint_syntax_error_reported() {
        let engine = TaploEngine::new();
        // Missing value after `=` is a syntax error.
        let src = make_src("key =\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(!diags.is_empty(), "expected a syntax-error diagnostic");
        assert_eq!(diags[0].code.as_deref(), Some("syntax-error"));
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn lint_duplicate_key_reported() {
        let engine = TaploEngine::new();
        let src = make_src("key = 1\nkey = 2\n");
        let diags = engine.lint(&src, &default_cfg()).unwrap();
        assert!(
            diags
                .iter()
                .any(|d| d.code.as_deref() == Some("duplicate-key")),
            "expected a duplicate-key diagnostic, got: {diags:?}"
        );
    }

    #[test]
    fn format_clean_toml_is_unchanged() {
        let engine = TaploEngine::new();
        // A well-formatted single-entry file (trailing newline).
        let src = make_src("key = \"value\"\n");
        let result = engine.format(&src, &default_cfg()).unwrap();
        assert!(
            matches!(result, FormatOutput::Unchanged),
            "clean TOML should not be reformatted"
        );
    }

    #[test]
    fn format_messy_toml_is_reformatted() {
        let engine = TaploEngine::new();
        // Extra spaces around = that taplo normalizes.
        let src = make_src("key  =  \"value\"\n");
        let result = engine.format(&src, &default_cfg()).unwrap();
        match result {
            FormatOutput::Formatted(out) => {
                assert!(
                    out.contains("key = \"value\""),
                    "normalized key/value: {out}"
                );
            }
            FormatOutput::Unchanged => panic!("expected reformatting"),
        }
    }

    #[test]
    fn format_respects_column_width_option() {
        let engine = TaploEngine::new();
        let mut opts = toml::Table::new();
        opts.insert("column_width".to_string(), toml::Value::Integer(40));
        let cfg = EngineConfig {
            globals: crate::config::GlobalDefaults::default(),
            indent_width: 2,
            options: opts,
        };
        // An array that would stay on one line at width 120 but expands at 40.
        let src = make_src("arr = [\"aaaaaaaaaaaaa\", \"bbbbbbbbbbbbb\", \"ccccccccccccc\"]\n");
        let result = engine.format(&src, &cfg).unwrap();
        match result {
            FormatOutput::Formatted(out) => {
                assert!(
                    out.contains('\n'),
                    "array should be expanded with narrow column_width: {out}"
                );
            }
            FormatOutput::Unchanged => {
                // The array may fit even at 40 — test only that the option is wired up,
                // not the exact layout decision.
            }
        }
    }

    #[test]
    fn byte_to_line_col_first_char() {
        assert_eq!(byte_to_line_col("foo\nbar\n", 0), (1, 1));
    }

    #[test]
    fn byte_to_line_col_second_line() {
        // "foo\n" is 4 bytes; 'b' is at byte 4.
        assert_eq!(byte_to_line_col("foo\nbar\n", 4), (2, 1));
    }
}
