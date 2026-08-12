//! oxc lint path: oxlint diagnostics for JS/TS/JSX/TSX and strict validation
//! for JSON/JSONC.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use oxc_allocator::Allocator;
use oxc_diagnostics::Severity as OxcSeverity;
use oxc_linter::{
    AllowWarnDeny, ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintFilter, LintOptions, LintService,
    LintServiceOptions, Linter, Message, PossibleFixes, RuntimeFileSystem,
};

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Edit, Severity, SourceFile, Span};
use crate::engines::rule_config::RuleSelection;
use crate::language::Language;

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

pub(super) fn lint_js(src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
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

    // oxlint carries the rule identity on the diagnostic's `OxcCode` — `scope` is
    // the plugin display name, `number` the rule name (`with_error_code`). It
    // replaced the old `Message::rule` field, whose `Display` (`scope(number)`)
    // is not the form we report. ~keep
    let code = match (&msg.error.code.scope, &msg.error.code.number) {
        (Some(plugin), Some(rule)) if plugin == "eslint" => Some(rule.to_string()),
        (Some(plugin), Some(rule)) => Some(format!("{plugin}/{rule}")),
        (None, Some(rule)) => Some(rule.to_string()),
        _ => None,
    };

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

pub(super) fn lint_json(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
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
}
