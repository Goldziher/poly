//! oxc backend (M2): JS, TS, JSX, TSX lint + format via `oxc_parser` /
//! `oxc_codegen`, plus JSON/JSONC format via `serde_json`.
//!
//! `oxc_linter` and `oxc_formatter` are not published to crates.io, so we wrap
//! the published crates only: parse errors are the lint diagnostics, and
//! `oxc_codegen` re-emits the AST as the format output.

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions, IndentChar};
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// Version string folded into the blake3 cache key.
/// Bump whenever the output of `lint` or `format` could change.
const VERSION: &str = "oxc_parser:0.137+codegen:0.137";

static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
];

/// oxc backend: wraps `oxc_parser` for lint diagnostics and `oxc_codegen` for
/// JS/TS formatting; uses `serde_json` for JSON/JSONC.
pub struct OxcEngine;

impl crate::engine::Engine for OxcEngine {
    fn name(&self) -> &'static str {
        "oxc"
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
        VERSION
    }

    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match src.language {
            Language::Json | Language::Jsonc => lint_json(src),
            _ => lint_js(src),
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match src.language {
            Language::Json | Language::Jsonc => format_json(src),
            _ => format_js(src, cfg),
        }
    }
}

// ── JS/TS helpers ────────────────────────────────────────────────────────────

fn source_type_for(lang: &Language) -> SourceType {
    match lang {
        Language::TypeScript => SourceType::ts(),
        Language::Tsx => SourceType::tsx(),
        Language::Jsx => SourceType::jsx(),
        _ => SourceType::mjs(),
    }
}

/// Byte offset → 1-based `(line, col)`.
fn offset_to_line_col(src: &str, offset: usize) -> (u32, u32) {
    let safe_offset = offset.min(src.len());
    let mut line: u32 = 1;
    let mut col: u32 = 1;
    for (i, ch) in src.char_indices() {
        if i >= safe_offset {
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

fn lint_js(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    let allocator = Allocator::new();
    let source_type = source_type_for(&src.language);
    let ret = Parser::new(&allocator, &src.content, source_type).parse();
    let diagnostics = ret
        .diagnostics
        .errors()
        .map(|diag| {
            // OxcDiagnostic derefs to OxcDiagnosticInner; .labels derefs to [LabeledSpan].
            let span = diag.labels.first().map(|label| {
                // oxc-miette ByteOffset is u32; cast to usize for offset_to_line_col.
                let start_offset = label.offset() as usize;
                let end_offset = start_offset + label.len() as usize;
                let (start_line, start_col) = offset_to_line_col(&src.content, start_offset);
                let (end_line, end_col) = offset_to_line_col(&src.content, end_offset);
                Span {
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                }
            });
            Diagnostic {
                engine: "oxc".to_owned(),
                code: Some("parse-error".to_owned()),
                message: diag.to_string(),
                severity: Severity::Error,
                span,
                fix: None,
            }
        })
        .collect();
    Ok(diagnostics)
}

fn format_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let source_type = source_type_for(&src.language);
    let ret = Parser::new(&allocator, &src.content, source_type).parse();

    // If the file has parse errors we cannot meaningfully reformat it.
    if ret.diagnostics.has_errors() {
        return Ok(FormatOutput::Unchanged);
    }

    let formatted = Codegen::new()
        .with_options(CodegenOptions {
            single_quote: false,
            indent_char: IndentChar::Space,
            indent_width: cfg.indent_width,
            ..CodegenOptions::default()
        })
        .build(&ret.program)
        .code;

    // Ensure final newline.
    let formatted = if formatted.ends_with('\n') {
        formatted
    } else {
        format!("{formatted}\n")
    };

    if formatted == src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(formatted))
    }
}

// ── JSON/JSONC helpers ────────────────────────────────────────────────────────

fn lint_json(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    let text = if src.language == Language::Jsonc {
        strip_jsonc_comments(&src.content)
    } else {
        src.content.clone()
    };

    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(_) => Ok(vec![]),
        Err(err) => {
            let line = err.line() as u32;
            let col = err.column() as u32;
            Ok(vec![Diagnostic {
                engine: "oxc".to_owned(),
                code: Some("parse-error".to_owned()),
                message: err.to_string(),
                severity: Severity::Error,
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

fn format_json(src: &SourceFile) -> anyhow::Result<FormatOutput> {
    // JSONC carries comments that a serde_json round-trip would discard, and
    // serde has no comment-preserving emitter. Never reformat JSONC — report it
    // unchanged rather than silently deleting comments.
    if src.language == Language::Jsonc {
        return Ok(FormatOutput::Unchanged);
    }

    // Key order is preserved via serde_json's `preserve_order` feature, so a
    // format never reshuffles object members.
    let value: serde_json::Value = match serde_json::from_str(&src.content) {
        Ok(v) => v,
        Err(_) => return Ok(FormatOutput::Unchanged),
    };

    let mut pretty = serde_json::to_string_pretty(&value)?;
    if !pretty.ends_with('\n') {
        pretty.push('\n');
    }

    if pretty == src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(pretty))
    }
}

/// Strip `//` and `/* */` comments from JSONC, preserving string contents and
/// character positions (comments are replaced with spaces so offsets stay valid).
fn strip_jsonc_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // String literal — copy verbatim until the closing `"`.
            '"' => {
                out.push('"');
                loop {
                    match chars.next() {
                        None => break,
                        Some('\\') => {
                            out.push('\\');
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        Some('"') => {
                            out.push('"');
                            break;
                        }
                        Some(c) => out.push(c),
                    }
                }
            }
            // Possible comment start.
            '/' => match chars.peek() {
                Some('/') => {
                    chars.next(); // consume second '/'
                    out.push(' ');
                    out.push(' ');
                    // Consume until newline (keep newline).
                    for c in chars.by_ref() {
                        if c == '\n' {
                            out.push('\n');
                            break;
                        } else {
                            out.push(' ');
                        }
                    }
                }
                Some('*') => {
                    chars.next(); // consume '*'
                    out.push(' ');
                    out.push(' ');
                    // Consume until '*/'.
                    let mut prev = ' ';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            out.push(' ');
                            break;
                        }
                        out.push(if c == '\n' { '\n' } else { ' ' });
                        prev = c;
                    }
                }
                _ => out.push('/'),
            },
            other => out.push(other),
        }
    }

    out
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::GlobalDefaults;
    use crate::engine::Engine;

    fn make_src(content: &str, lang: Language) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.js"),
            language: lang,
            content: content.to_owned(),
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
    fn valid_js_produces_no_diagnostics() {
        let src = make_src("const x = 1;\n", Language::JavaScript);
        let diags = lint_js(&src).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn invalid_js_produces_parse_error() {
        let src = make_src("const x = {\n  a: 1,\nconst y = 2;\n", Language::JavaScript);
        let diags = lint_js(&src).unwrap();
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn format_js_normalizes_spacing() {
        let src = make_src("const x={a:1,b:2};\n", Language::JavaScript);
        let cfg = default_cfg();
        let out = format_js(&src, &cfg).unwrap();
        assert!(matches!(out, FormatOutput::Formatted(_)));
    }

    #[test]
    fn format_js_returns_unchanged_for_already_formatted() {
        // Already formatted output should round-trip as Unchanged.
        let src = make_src("const x = {\n  a: 1,\n  b: 2,\n};\n", Language::JavaScript);
        let cfg = default_cfg();
        // Run once to get canonical form.
        let first = match format_js(&src, &cfg).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => src.content.clone(),
        };
        let src2 = make_src(&first, Language::JavaScript);
        let second = format_js(&src2, &cfg).unwrap();
        assert!(
            matches!(second, FormatOutput::Unchanged),
            "second pass should be Unchanged; got: {second:?}"
        );
    }

    #[test]
    fn valid_json_produces_no_diagnostics() {
        let src = make_src(r#"{"a":1}"#, Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn invalid_json_produces_parse_error() {
        let src = make_src(r#"{"a":1,}"#, Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn jsonc_with_comments_is_valid() {
        let src = make_src("{\n  // comment\n  \"a\": 1\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty(), "got diags: {diags:?}");
    }

    #[test]
    fn strip_jsonc_preserves_string_slashes() {
        let input = r#"{"url": "http://example.com"}"#;
        let stripped = strip_jsonc_comments(input);
        assert_eq!(stripped, input);
    }

    #[test]
    fn engine_metadata() {
        let engine = OxcEngine;
        assert_eq!(engine.name(), "oxc");
        assert!(engine.capabilities().lint);
        assert!(engine.capabilities().format);
        assert!(!engine.capabilities().fix);
    }
}
