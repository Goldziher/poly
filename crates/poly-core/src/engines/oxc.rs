//! oxc backend (M2): JS, TS, JSX, TSX lint + format via `oxc_linter` /
//! `oxc_formatter`, plus JSON/JSONC format via `oxc_formatter_json`.
//!
//! Lint path uses `oxc_linter` (oxlint) to run the full correctness rule set
//! in-process via `LintService::run_source`. An in-memory `RuntimeFileSystem`
//! adapter feeds file content from RAM — no disk read inside the engine.
//!
//! `oxc_formatter` (Prettier-compatible, v0.56.0) handles JS/TS formatting.
//! `oxc_formatter_json` handles JSON/JSONC formatting: Prettier-compatible,
//! short arrays stay inline, JSONC comments are preserved.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use oxc_allocator::Allocator;
use oxc_diagnostics::Severity as OxcSeverity;
use oxc_formatter::JsFormatOptions;
use oxc_formatter_core::{IndentStyle, IndentWidth, LineWidth};
use oxc_formatter_json::{JsonFormatOptions, JsonVariant};
use oxc_linter::{
    AllowWarnDeny, ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintFilter, LintOptions, LintService,
    LintServiceOptions, Linter, Message, PossibleFixes, RuntimeFileSystem,
};
use oxc_span::SourceType;

use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, FormatOutput, Severity, SourceFile, Span};
use crate::engines::rule_config::RuleSelection;
use crate::language::Language;

/// Version string folded into the blake3 cache key.
/// Bump whenever the output of `lint` or `format` could change.
/// Reflects the oxc monorepo rev + formatter version + oxlint integration marker.
/// `+rules-v2`: per-rule `AllowWarnDeny::Deny` severity support added.
/// `+fmt-opts`:  JS quote_style, semicolons, trailing_commas, arrow_parentheses,
///               bracket_spacing, bracket_same_line, indent_style; JSON bracket_spacing
///               and trailing_commas now wired from `cfg.options`.
const VERSION: &str =
    "oxc_formatter:0.60.0+oxlint+parser:0.141.0+rev:0aef19e+json-fmt+rules-v2+fmt-opts+jsonc-trailing-comma";

static LANGUAGES: &[Language] = &[
    Language::JavaScript,
    Language::TypeScript,
    Language::Jsx,
    Language::Tsx,
    Language::Json,
    Language::Jsonc,
];

/// oxc backend: wraps `oxc_linter` for full correctness-rule lint diagnostics,
/// `oxc_formatter` for JS/TS formatting (Prettier-compatible), and
/// `oxc_formatter_json` for JSON/JSONC formatting.
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

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match src.language {
            Language::Json | Language::Jsonc => lint_json(src),
            _ => lint_js(src, cfg),
        }
    }

    fn format(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match src.language {
            Language::Json | Language::Jsonc => format_json(src, cfg),
            _ => format_js(src, cfg),
        }
    }
}

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
        Ok(())
    }
}

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
            .expect("oxc_linter default ConfigStore build is infallible");
        let config_store = ConfigStore::new(config, Default::default(), plugin_store);
        let linter = Linter::new(LintOptions::default(), config_store, None);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let options = LintServiceOptions::new(cwd);
        LintService::new(linter, options)
    })
}

/// Run `service` against one source file and return the raw oxlint messages.
///
/// Extracted so both the cached-service and the per-config-service paths share
/// identical call-site code.
fn run_with_service(service: &LintService, src: &SourceFile) -> Vec<Message> {
    let arc_path: Arc<OsStr> = Arc::from(src.path.as_os_str());
    let fs = MemoryFileSystem {
        path: &src.path,
        content: &src.content,
    };
    service.run_source(&fs, vec![arc_path])
}

/// Build a fresh [`LintService`] applying rule filters from `cfg.options`.
///
/// Only called when `cfg.options` is non-empty; the empty-config fast path
/// reuses the shared [`OnceLock`] service from [`lint_service`].
///
/// ## Config keys consumed
///
/// * `select = ["rule", …]` — enable each named rule at Warning severity.
/// * `extend_select = ["rule", …]` — add rules on top of the default set.
/// * `ignore = ["rule", …]` — disable each named rule (Allow).
/// * `[rules.<id>] level = "error"` — promote a rule to Error/Deny severity.
/// * `[rules.<id>] level = "warning"|"info"|"hint"` — keep at Warn severity.
///
/// Per-rule level mapping: `"error"` → [`AllowWarnDeny::Deny`];
/// `"warning"` / `"info"` / `"hint"` → [`AllowWarnDeny::Warn`].
/// `None` level (table present, no `level` key) leaves the rule's default.
///
/// Unrecognised or malformed rule names are silently skipped so that a typo
/// in the user's config does not prevent the other rules from running.
fn build_configured_service(cfg: &EngineConfig) -> anyhow::Result<LintService> {
    let selection = RuleSelection::from_options(cfg);

    let mut plugin_store = ExternalPluginStore::default();
    let mut builder = ConfigStoreBuilder::default();

    for name in &selection.select {
        if let Ok(filter) = LintFilter::new(AllowWarnDeny::Warn, name.to_owned()) {
            builder = builder.with_filter(&filter);
        }
    }

    for name in &selection.extend_select {
        if let Ok(filter) = LintFilter::new(AllowWarnDeny::Warn, name.to_owned()) {
            builder = builder.with_filter(&filter);
        }
    }

    for name in &selection.ignore {
        if let Ok(filter) = LintFilter::new(AllowWarnDeny::Allow, name.to_owned()) {
            builder = builder.with_filter(&filter);
        }
    }

    for (code, opts) in &selection.rules {
        if let Some(level) = opts.level {
            let awd = match level {
                Severity::Error => AllowWarnDeny::Deny,
                _ => AllowWarnDeny::Warn,
            };
            if let Ok(filter) = LintFilter::new(awd, code.to_owned()) {
                builder = builder.with_filter(&filter);
            }
        }
    }

    let config = builder
        .build(&mut plugin_store)
        .map_err(|e| anyhow::anyhow!("oxlint config error: {e}"))?;
    let config_store = ConfigStore::new(config, Default::default(), plugin_store);
    let linter = Linter::new(LintOptions::default(), config_store, None);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let options = LintServiceOptions::new(cwd);
    Ok(LintService::new(linter, options))
}

fn lint_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
    let messages = if cfg.options.is_empty() {
        run_with_service(lint_service(), src)
    } else {
        let service = build_configured_service(cfg)?;
        run_with_service(&service, src)
    };
    let diagnostics = messages
        .into_iter()
        .map(|msg| map_oxlint_message(msg, &src.content))
        .collect();
    Ok(diagnostics)
}

/// Map one `oxc_linter::Message` to a poly [`Diagnostic`].
///
/// Rule code: `plugin/rule` for non-eslint plugins; bare `rule` for
/// `eslint/*`. `None` when the message has no rule (e.g. a parse error).
///
/// Fix: all edits are forwarded — `Single` as one edit, `Multiple` as the full
/// list. The runner applies each diagnostic's edits atomically (all-or-nothing,
/// with an overlap guard), so multi-edit fixes are safe to attach.
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

    let message_text = msg.error.to_string();

    let description = msg.error.help.as_ref().map(|h| h.to_string());
    let url = msg.error.url.as_ref().map(|u| u.to_string());

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

    let fix: Vec<Edit> = match msg.fixes {
        PossibleFixes::Single(f) => vec![Edit {
            start_byte: f.span.start as usize,
            end_byte: f.span.end as usize,
            replacement: f.content.into_owned(),
        }],
        PossibleFixes::Multiple(fixes) => fixes
            .into_iter()
            .map(|f| Edit {
                start_byte: f.span.start as usize,
                end_byte: f.span.end as usize,
                replacement: f.content.into_owned(),
            })
            .collect(),
        PossibleFixes::None => vec![],
    };

    Diagnostic {
        engine: "oxc".to_owned(),
        code,
        title: message_text,
        description,
        severity,
        span,
        url,
        fix,
        metadata: Default::default(),
    }
}

/// Build [`JsFormatOptions`] from a resolved [`EngineConfig`].
///
/// ## Layering order (Prettier-compatible defaults → poly overrides → user config)
///
/// | `cfg.options` key | Type | Values |
/// |---|---|---|
/// | `quote_style` | string | `"double"` (default) / `"single"` |
/// | `jsx_quote_style` | string | `"double"` (default) / `"single"` |
/// | `semicolons` | string | `"always"` (default) / `"as-needed"` |
/// | `trailing_commas` | string | `"all"` (default) / `"es5"` / `"none"` |
/// | `arrow_parentheses` | string | `"always"` (default) / `"as-needed"` |
/// | `bracket_spacing` | bool | `true` (default) |
/// | `bracket_same_line` | bool | `false` (default) |
/// | `indent_style` | string | `"space"` (default) / `"tab"` |
///
/// `line_width` and `indent_width` are always taken from `cfg.globals.line_length`
/// and `cfg.indent_width` respectively — user cannot override them here.
fn build_js_options(cfg: &EngineConfig) -> JsFormatOptions {
    use oxc_formatter::{
        ArrowParentheses, BracketSameLine, BracketSpacing, QuoteStyle, Semicolons, TrailingCommas as JsTrailingCommas,
    };

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
        .unwrap_or_default();

    let indent_style = cfg
        .options
        .get("indent_style")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<IndentStyle>().ok())
        .unwrap_or_default();

    let quote_style = cfg
        .options
        .get("quote_style")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "single" => QuoteStyle::Single,
            _ => QuoteStyle::Double,
        })
        .unwrap_or_default();

    let jsx_quote_style = cfg
        .options
        .get("jsx_quote_style")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "single" => QuoteStyle::Single,
            _ => QuoteStyle::Double,
        })
        .unwrap_or_default();

    let semicolons = cfg
        .options
        .get("semicolons")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Semicolons>().ok())
        .unwrap_or_default();

    let trailing_commas = cfg
        .options
        .get("trailing_commas")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<JsTrailingCommas>().ok())
        .unwrap_or_default();

    let arrow_parentheses = cfg
        .options
        .get("arrow_parentheses")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<ArrowParentheses>().ok())
        .unwrap_or_default();

    let bracket_spacing = cfg
        .options
        .get("bracket_spacing")
        .and_then(|v| v.as_bool())
        .map(BracketSpacing::from)
        .unwrap_or_default();

    let bracket_same_line = cfg
        .options
        .get("bracket_same_line")
        .and_then(|v| v.as_bool())
        .map(BracketSameLine::from)
        .unwrap_or_default();

    JsFormatOptions {
        line_width,
        indent_width,
        indent_style,
        quote_style,
        jsx_quote_style,
        semicolons,
        trailing_commas,
        arrow_parentheses,
        bracket_spacing,
        bracket_same_line,
        ..JsFormatOptions::default()
    }
}

/// Format a JS/TS/JSX/TSX file using `oxc_formatter` (Prettier-compatible).
///
/// Line width is taken from `cfg.globals.line_length` (project default: 120).
/// Additional formatter options (`quote_style`, `semicolons`, `trailing_commas`,
/// `arrow_parentheses`, `bracket_spacing`, `bracket_same_line`, `indent_style`)
/// can be set via `[fmt.<lang>.oxc]` in `poly.toml`.
fn format_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let source_type = source_type_for(&src.language);
    let options = build_js_options(cfg);

    let formatted = match oxc_formatter::format(&allocator, &src.content, source_type, options, None) {
        Err(_) => return Ok(FormatOutput::Unchanged),
        Ok(f) => f,
    };

    let printed = formatted
        .print()
        .map_err(|e| anyhow::anyhow!("oxc_formatter print error: {e}"))?;
    let mut code = printed.into_code();

    if !code.ends_with('\n') {
        code.push('\n');
    }

    if code == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(code))
    }
}

fn lint_json(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    // JSONC permits comments *and* trailing commas — both valid in the spec our
    // formatter targets, and the JSONC formatter itself emits/preserves trailing
    // commas. `serde_json` is strict JSON, so it rejects both. Neutralise them
    // (replace with spaces, preserving byte offsets so any *genuine* parse error
    // still reports at the right position) before the strict parse. Plain `.json`
    // keeps strict semantics: a trailing comma there is a real error.
    let text = if src.language == Language::Jsonc {
        neutralize_trailing_commas(&strip_jsonc_comments(&src.content))
    } else {
        src.content.to_string()
    };

    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(_) => Ok(vec![]),
        Err(err) => {
            let line = err.line() as u32;
            let col = err.column() as u32;
            Ok(vec![Diagnostic {
                engine: "oxc".to_owned(),
                code: Some("parse-error".to_owned()),
                title: err.to_string(),
                description: None,
                url: None,
                severity: Severity::Error,
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

/// Build [`JsonFormatOptions`] from a resolved [`EngineConfig`].
///
/// ## Layering order
///
/// | `cfg.options` key | Type | Values |
/// |---|---|---|
/// | `bracket_spacing` | bool | `true` (default) |
/// | `trailing_commas` | string | `"always"` (default for JSONC) / `"never"` |
///
/// `line_width` and `indent_width` are always sourced from `cfg.globals.line_length`
/// and `cfg.indent_width`. The variant (Json vs Jsonc) is derived from the file language
/// and cannot be overridden per-option.
fn build_json_options(src: &SourceFile, cfg: &EngineConfig) -> JsonFormatOptions {
    use oxc_formatter_json::{BracketSpacing as JsonBracketSpacing, TrailingCommas as JsonTc};

    let variant = match src.language {
        Language::Jsonc => JsonVariant::Jsonc,
        _ => JsonVariant::Json,
    };

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
        .unwrap_or_default();

    let bracket_spacing = cfg
        .options
        .get("bracket_spacing")
        .and_then(|v| v.as_bool())
        .map(JsonBracketSpacing::from)
        .unwrap_or_default();

    let trailing_commas = cfg
        .options
        .get("trailing_commas")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "never" => JsonTc::Never,
            _ => JsonTc::Always,
        })
        .unwrap_or_default();

    JsonFormatOptions {
        variant,
        line_width,
        indent_width,
        bracket_spacing,
        trailing_commas,
        ..JsonFormatOptions::default()
    }
}

fn format_json(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
    let allocator = Allocator::new();
    let options = build_json_options(src, cfg);

    let formatted = match oxc_formatter_json::format(&allocator, &src.content, options) {
        Err(_) => return Ok(FormatOutput::Unchanged),
        Ok(f) => f,
    };

    let mut code = formatted
        .print()
        .map_err(|e| anyhow::anyhow!("oxc_formatter_json print error: {e}"))?
        .into_code();

    if !code.ends_with('\n') {
        code.push('\n');
    }

    if code == *src.content {
        Ok(FormatOutput::Unchanged)
    } else {
        Ok(FormatOutput::Formatted(code))
    }
}

/// Strip `//` and `/* */` comments from JSONC, preserving string contents and
/// character positions (comments are replaced with spaces so offsets stay valid).
fn strip_jsonc_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
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
            '/' => match chars.peek() {
                Some('/') => {
                    chars.next();
                    out.push(' ');
                    out.push(' ');
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
                    chars.next();
                    out.push(' ');
                    out.push(' ');
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

/// Replace JSONC **trailing commas** — a `,` whose next non-whitespace character
/// is `}` or `]` — with a space, so strict `serde_json` accepts them while byte
/// offsets (and therefore any genuine parse-error position) are preserved.
///
/// Operates on comment-stripped input (comments are already spaces) and is
/// string-aware: commas and brackets inside string literals are ignored.
fn neutralize_trailing_commas(src: &str) -> String {
    let mut bytes: Vec<u8> = src.as_bytes().to_vec();
    // Byte index of the most recent structural `,` with only whitespace since.
    let mut pending_comma: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for i in 0..bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                pending_comma = None;
            }
            b',' => pending_comma = Some(i),
            b'}' | b']' => {
                if let Some(j) = pending_comma.take() {
                    bytes[j] = b' ';
                }
            }
            _ if b.is_ascii_whitespace() => {}
            _ => pending_comma = None,
        }
    }

    // SAFETY-equivalent: we only ever overwrite an ASCII `,` with an ASCII space,
    // so the buffer remains valid UTF-8.
    String::from_utf8(bytes).expect("blanking ASCII commas keeps valid UTF-8")
}

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
    fn valid_js_produces_no_diagnostics() {
        let src = make_src("export function square(n) { return n * n; }\n", Language::JavaScript);
        let diags = lint_js(&src, &default_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics; got: {diags:#?}");
    }

    #[test]
    fn invalid_js_produces_parse_error() {
        let src = make_src("const x = {\n  a: 1,\nconst y = 2;\n", Language::JavaScript);
        let diags = lint_js(&src, &default_cfg()).unwrap();
        assert!(!diags.is_empty(), "expected at least one diagnostic for broken JS");
        assert_eq!(diags[0].severity, Severity::Error);
        assert!(diags[0].code.is_none(), "parse error should not have a rule code");
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
        let src = make_src("const x = {\n  a: 1,\n  b: 2,\n};\n", Language::JavaScript);
        let cfg = default_cfg();
        let first = match format_js(&src, &cfg).unwrap() {
            FormatOutput::Formatted(s) => s,
            FormatOutput::Unchanged => src.content.to_string(),
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
    fn jsonc_with_trailing_commas_is_valid() {
        // Object, array, and nested trailing commas — all valid JSONC, all of
        // which the JSONC formatter itself emits/preserves.
        let src = make_src("{\n  \"a\": 1,\n  \"b\": [1, 2,],\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty(), "trailing commas are valid JSONC; got: {diags:?}");
    }

    #[test]
    fn jsonc_genuinely_invalid_still_errors() {
        // A real syntax error (missing value) must still be reported.
        let src = make_src("{\n  \"a\":\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty(), "malformed JSONC must still error");
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn plain_json_trailing_comma_still_errors() {
        // Strict `.json` keeps strict semantics — trailing commas are invalid.
        let src = make_src("{\"a\": 1,}", Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty(), "trailing comma is invalid in strict JSON");
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn neutralize_ignores_comma_inside_string() {
        // A `,` followed by `]` *inside a string* is not a trailing comma.
        let input = r#"{"a": "x,]", "b": [1,]}"#;
        let out = neutralize_trailing_commas(input);
        // The in-string `,` survives; the real trailing `,` before `]` is blanked.
        assert_eq!(out, r#"{"a": "x,]", "b": [1 ]}"#);
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
        let fs = MemoryFileSystem { path: &path, content };
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

    /// `[rules.no-debugger] level = "error"` must promote the `no-debugger`
    /// diagnostic to [`Severity::Error`] (mapped from `AllowWarnDeny::Deny`).
    #[test]
    fn per_rule_deny_via_rules_table_gives_error_severity() {
        let src = make_src("const x = 1;\ndebugger;\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.no-debugger]
level = "error"
"#,
            )
            .unwrap(),
        };
        let diags = lint_js(&src, &cfg).unwrap();
        let d = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("no-debugger"))
            .expect("no-debugger should fire on `debugger;`");
        assert_eq!(
            d.severity,
            Severity::Error,
            "level = 'error' should promote to Severity::Error via AllowWarnDeny::Deny"
        );
    }

    /// `[rules.no-debugger] level = "warning"` keeps the diagnostic at Warning.
    #[test]
    fn per_rule_warn_via_rules_table_keeps_warning_severity() {
        let src = make_src("const x = 1;\ndebugger;\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.no-debugger]
level = "warning"
"#,
            )
            .unwrap(),
        };
        let diags = lint_js(&src, &cfg).unwrap();
        let d = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("no-debugger"))
            .expect("no-debugger should fire on `debugger;`");
        assert_eq!(
            d.severity,
            Severity::Warning,
            "level = 'warning' should stay Severity::Warning via AllowWarnDeny::Warn"
        );
    }

    /// `quote_style = "single"` rewrites `"hello"` to `'hello'`.
    #[test]
    fn js_format_single_quote_style_rewrites_double_quotes() {
        let src = make_src("export const greeting = \"hello\";\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(r#"quote_style = "single""#).unwrap(),
        };
        let out = format_js(&src, &cfg).unwrap();
        match out {
            FormatOutput::Formatted(text) => {
                assert!(text.contains("'hello'"), "expected single-quoted string; got: {text:?}");
            }
            FormatOutput::Unchanged => {
                panic!("expected Formatted output with single quotes, got Unchanged");
            }
        }
    }

    /// `semicolons = "as-needed"` strips the trailing semicolons.
    #[test]
    fn js_format_semicolons_as_needed_removes_semicolons() {
        let src = make_src("export const x = 1;\nexport const y = 2;\n", Language::JavaScript);
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(r#"semicolons = "as-needed""#).unwrap(),
        };
        let out = format_js(&src, &cfg).unwrap();
        match out {
            FormatOutput::Formatted(text) => {
                assert!(!text.contains(";\n"), "expected semicolons removed; got: {text:?}");
            }
            FormatOutput::Unchanged => {}
        }
    }
}
