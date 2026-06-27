//! oxc backend (M2): JS, TS, JSX, TSX lint + format via `oxc_linter` /
//! `oxc_formatter`, plus JSON/JSONC format via `serde_json`.
//!
//! Lint path uses `oxc_linter` (oxlint) to run the full correctness rule set
//! in-process via `LintService::run_source`. An in-memory `RuntimeFileSystem`
//! adapter feeds file content from RAM — no disk read inside the engine.
//!
//! `oxc_formatter` (Prettier-compatible, v0.56.0) handles JS/TS formatting.
//! `serde_json` handles JSON/JSONC.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use oxc_allocator::Allocator;
use oxc_diagnostics::Severity as OxcSeverity;
use oxc_formatter::JsFormatOptions;
use oxc_formatter_core::{IndentWidth, LineWidth};
use oxc_linter::{
    ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintOptions, LintService,
    LintServiceOptions, Linter, Message, PossibleFixes, RuntimeFileSystem,
};
use oxc_span::SourceType;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, FormatOutput, Severity, SourceFile, Span};
use crate::language::Language;

/// Version string folded into the blake3 cache key.
/// Bump whenever the output of `lint` or `format` could change.
/// Reflects the oxc monorepo rev + formatter version + oxlint integration marker.
const VERSION: &str = "oxc_formatter:0.56.0+oxlint+parser:0.56.0+rev:5762638";

static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
];

/// oxc backend: wraps `oxc_linter` for full correctness-rule lint diagnostics,
/// `oxc_formatter` for JS/TS formatting (Prettier-compatible), and `serde_json`
/// for JSON/JSONC.
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

// ── in-memory RuntimeFileSystem adapter ─────────────────────────────────────

/// Feeds `oxc_linter`'s parser with file content from RAM.
/// `read_to_arena_str` copies `content` into the oxc arena allocator — no disk
/// access ever occurs inside the engine.
struct MemoryFileSystem<'a> {
    path: &'a Path,
    content: &'a str,
}

impl RuntimeFileSystem for MemoryFileSystem<'_> {
    fn read_to_arena_str<'arena>(
        &self,
        path: &Path,
        allocator: &'arena Allocator,
    ) -> Result<&'arena str, std::io::Error> {
        if path == self.path {
            Ok(allocator.alloc_str(self.content))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "path not available in memory",
            ))
        }
    }

    fn write_file(&self, _path: &Path, _content: &str) -> Result<(), std::io::Error> {
        // We never write back through the linter.
        Ok(())
    }
}

// ── LintService construction (lazily initialised, reused across files) ───────

/// Returns the lazily-initialised shared [`LintService`] configured with
/// oxlint's default correctness rule set.
///
/// Building the service (rule table + allocator pool) is expensive; the
/// `OnceLock` ensures the cost is paid at most once per process.
///
/// # Panics
/// Panics on first call if the default `ConfigStore` cannot be built — this is
/// a compile-time invariant that cannot fail with no external inputs.
fn lint_service() -> &'static LintService {
    static SERVICE: OnceLock<LintService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let mut plugin_store = ExternalPluginStore::default();
        let config = ConfigStoreBuilder::default()
            .build(&mut plugin_store)
            // SAFETY: ConfigStoreBuilder::default().build() with no external
            // configuration is infallible — it only reads built-in rule defs.
            .expect("oxc_linter default ConfigStore build is infallible");
        let config_store = ConfigStore::new(config, Default::default(), plugin_store);
        let linter = Linter::new(LintOptions::default(), config_store, None);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let options = LintServiceOptions::new(cwd);
        LintService::new(linter, options)
    })
}

// ── lint_js: run oxlint correctness rules in-process ─────────────────────────

fn lint_js(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    let service = lint_service();
    let arc_path: Arc<OsStr> = Arc::from(src.path.as_os_str());
    let fs = MemoryFileSystem {
        path: &src.path,
        content: &src.content,
    };
    let messages = service.run_source(&fs, vec![arc_path]);
    let diagnostics = messages
        .into_iter()
        .map(|msg| map_oxlint_message(msg, &src.content))
        .collect();
    Ok(diagnostics)
}

/// Map one `oxc_linter::Message` to a polylint [`Diagnostic`].
///
/// Rule code: `plugin/rule` for non-eslint plugins; bare `rule` for
/// `eslint/*`. `None` when the message has no rule (e.g. a parse error).
///
/// Fix: only attached for `PossibleFixes::Single` (one contiguous edit).
/// Multi-edit fixes are dropped to avoid partial application corrupting
/// the source.
fn map_oxlint_message(msg: Message, content: &str) -> Diagnostic {
    let severity = match msg.error.severity {
        OxcSeverity::Error => Severity::Error,
        OxcSeverity::Warning => Severity::Warning,
        OxcSeverity::Advice => Severity::Info,
    };

    let code = msg.rule.as_ref().map(|r| {
        if r.plugin_name == "eslint" {
            r.rule_name.to_string()
        } else {
            format!("{}/{}", r.plugin_name, r.rule_name)
        }
    });

    // `Display for OxcDiagnostic` formats as the primary message string.
    let message_text = msg.error.to_string();

    let start = msg.span.start as usize;
    let end = msg.span.end as usize;
    let (start_line, start_col) = offset_to_line_col(content, start);
    let (end_line, end_col) = offset_to_line_col(content, end);
    let span = Some(Span {
        start_line,
        start_col,
        end_line,
        end_col,
    });

    // Only map a fix when there is exactly one edit; skip multi-edit fixes to
    // avoid partially applying an incomplete set of changes.
    let fix = if let PossibleFixes::Single(f) = msg.fixes {
        Some(Edit {
            start_byte: f.span.start as usize,
            end_byte: f.span.end as usize,
            replacement: f.content.into_owned(),
        })
    } else {
        None
    };

    Diagnostic {
        engine: "oxc".to_owned(),
        code,
        message: message_text,
        severity,
        span,
        fix,
        metadata: Default::default(),
    }
}

/// Format a JS/TS/JSX/TSX file using `oxc_formatter` (Prettier-compatible).
///
/// Line width is taken from `cfg.globals.line_length` (project default: 120).
/// oxfmt's own default is 100; we override to 120 per polylint's opinionated
/// layer. Indent width comes from `cfg.indent_width` (default: 2).
fn format_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let source_type = source_type_for(&src.language);

    // Line width from config, clamped to a valid value.
    let line_width = u16::try_from(cfg.globals.line_length)
        .ok()
        .and_then(|w| LineWidth::try_from(w).ok())
        .unwrap_or_else(|| {
            // SAFETY: 120 is always in [LineWidth::MIN, LineWidth::MAX].
            LineWidth::try_from(120u16).expect("120 is a valid LineWidth")
        });

    let indent_width = u8::try_from(cfg.indent_width)
        .ok()
        .and_then(|w| IndentWidth::try_from(w).ok())
        .unwrap_or_default(); // default is 2

    let options = JsFormatOptions {
        line_width,
        indent_width,
        ..JsFormatOptions::default()
    };

    // format() parses internally; returns Err on the first parse error.
    let formatted =
        match oxc_formatter::format(&allocator, &src.content, source_type, options, None) {
            // Cannot meaningfully reformat a file with parse errors.
            Err(_) => return Ok(FormatOutput::Unchanged),
            Ok(f) => f,
        };

    let printed = formatted
        .print()
        .map_err(|e| anyhow::anyhow!("oxc_formatter print error: {e}"))?;
    let mut code = printed.into_code();

    // Ensure a trailing newline.
    if !code.ends_with('\n') {
        code.push('\n');
    }

    if code == src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(code))
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
                metadata: Default::default(),
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
        // Export the function so it is considered "used" by no-unused-vars.
        let src = make_src(
            "export function square(n) { return n * n; }\n",
            Language::JavaScript,
        );
        let diags = lint_js(&src).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics; got: {diags:#?}");
    }

    #[test]
    fn invalid_js_produces_parse_error() {
        let src = make_src("const x = {\n  a: 1,\nconst y = 2;\n", Language::JavaScript);
        let diags = lint_js(&src).unwrap();
        assert!(
            !diags.is_empty(),
            "expected at least one diagnostic for broken JS"
        );
        // oxlint wraps parse errors with Error severity; no rule is associated.
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(
            diags[0].code.is_none(),
            "parse error should not have a rule code"
        );
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
        // Run once to get the canonical Prettier-style form, then verify
        // the second pass is idempotent (Unchanged).
        let src = make_src("const x = {\n  a: 1,\n  b: 2,\n};\n", Language::JavaScript);
        let cfg = default_cfg();
        // First pass: may reformat (e.g. collapse to single line).
        let first = match format_js(&src, &cfg).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => src.content.clone(),
        };
        // Second pass: must be idempotent.
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
        // Language::Jsonc — strip_jsonc_comments is called before serde_json parse.
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

    /// Parser used by oxlint still needs an Allocator; verify it works
    /// with our MemoryFileSystem adapter.
    #[test]
    fn memory_fs_returns_source_for_matching_path() {
        let path = PathBuf::from("test.ts");
        let content = "const x: number = 1;\n";
        let allocator = Allocator::new();
        let fs = MemoryFileSystem {
            path: &path,
            content,
        };
        let result = fs.read_to_arena_str(&path, &allocator);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), content);
    }

    #[test]
    fn memory_fs_errors_on_unknown_path() {
        let path = PathBuf::from("test.ts");
        let allocator = Allocator::new();
        let fs = MemoryFileSystem {
            path: &path,
            content: "const x = 1;\n",
        };
        let other = PathBuf::from("other.ts");
        let result = fs.read_to_arena_str(&other, &allocator);
        assert!(result.is_err());
    }
}
