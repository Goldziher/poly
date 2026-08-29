//! oxc lint path: oxlint diagnostics for JS/TS/JSX/TSX and strict validation
//! for JSON/JSONC.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use oxc_allocator::Allocator;
use oxc_diagnostics::Severity as OxcSeverity;
use oxc_linter::{
    AllowWarnDeny, ConfigStore, ConfigStoreBuilder, ExternalPluginStore, LintFilter, LintOptions, LintService,
    LintServiceOptions, Linter, Message, Oxlintrc, PossibleFixes, RuntimeFileSystem,
};

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Edit, Severity, SourceFile, Span};
use crate::engines::rule_config::{RuleOptions, RuleSelection};
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

/// The opinionated filters layered on top of oxlint's own default.
///
/// `ConfigStoreBuilder::default()` upstream means the **`correctness` category
/// only**, which on real TypeScript reports almost nothing. These filters widen
/// it, all at Warning severity:
///
/// * `suspicious` / `pedantic` — the two categories that hold the actual
///   code-quality rules (`no-empty`, `eqeqeq`, `max-depth`, …). `restriction`,
///   `style` and `nursery` stay off: they are opinion, not defect detection.
/// * three named `restriction` rules that *are* defect detection — the escape
///   hatches out of the type system (`any`, `!`) and the debug `console` call
///   that should not have shipped. Named individually because enabling the
///   whole `restriction` category would bury them.
/// * `complexity` — also `restriction`, but its default threshold is exactly
///   20, the same cyclomatic-complexity budget poly's quality tier applies to
///   every other language. Enabling it here is what lets JS/TS *defer*
///   cyclomatic complexity to oxlint instead of reimplementing the metric.
///   Measured at 21 findings on hand-written source across a 20-repository
///   corpus (55 raw, of which 34 land in vendored/minified bundles that each
///   repo's own `[discovery] exclude` already drops); every one names a
///   genuinely branchy function — `optimizeDocument` at 43, `generateFromZodType`
///   at 39, `getSearchResultAllProperties` at 35 — with no false positives in
///   the hand-read set.
///
/// Applied by both [`lint_service`] (the no-config fast path) and
/// [`build_configured_service`] (the user-config path) so the two cannot drift.
/// User filters are applied *after* these, so `ignore` still wins.
const DEFAULT_LINT_FILTERS: &[&str] = &[
    "suspicious",
    "pedantic",
    "typescript/no-explicit-any",
    "typescript/no-non-null-assertion",
    "no-console",
    "complexity",
];

/// Members of the categories above that are turned back off by default.
///
/// Each was measured against a six-repository corpus (202 JS/TS files) and
/// hand-read; the counts quoted are from that run. Re-enable any of them with
/// `extend_select = ["<rule>"]` in the engine's config table.
///
/// - **no-underscore-dangle** — 608 findings, 3.0 per file, and 92% of the
///   sampled ones in production code. `_privateField` is a deliberate,
///   universal JavaScript convention; the rule detects no defect.
/// - **max-lines-per-function** — 131 findings across four repos at oxlint's
///   50-line default, which every `describe()` block in a test file exceeds.
/// - **max-lines** — 51 findings at oxlint's 300-line-per-file default, a
///   threshold poly does not endorse anywhere else (its own cap is 1000).
/// - **max-classes-per-file** — 27 findings across five repos (21 once each
///   repo's own `[discovery] exclude` drops generated wasm-bindgen and minified
///   bundles). All 27 were hand-read and **none** is a defect: they are error
///   taxonomies (`errors.ts` with 5 and with 50 sibling `Error` subclasses),
///   test files declaring their own mocks, `.d.ts` ambient stubs, and cohesive
///   module groups (`SearchStream`/`ExtractionStream`/`MetadataStream`). The
///   rule's default `max: 1` asserts one class per file — a position poly holds
///   nowhere else, and one that splitting a 50-case error hierarchy across 50
///   files would only make worse.
///
/// `max-nested-callbacks` (also `pedantic`, default 10) was measured the same
/// way and stays **on**: 0 findings corpus-wide, and a synthetic 15-deep fixture
/// confirms it is live rather than silently inert. A rule that costs nothing and
/// catches real callback pyramids earns its place.
const DEFAULT_ALLOWED_RULES: &[&str] = &[
    "no-underscore-dangle",
    "max-lines-per-function",
    "max-lines",
    "max-classes-per-file",
];

/// Layer [`DEFAULT_LINT_FILTERS`] (Warn) then [`DEFAULT_ALLOWED_RULES`] (Allow) onto
/// `builder`, in that order so the allow-list wins for rules present in both.
///
/// Extracted so both [`opinionated_builder`] (the no-config fast path's base) and
/// [`build_configured_service`] (which seeds its base from
/// [`ConfigStoreBuilder::from_oxlintrc`] instead, to carry per-rule options) apply
/// byte-for-byte the same opinionated layer — one source of truth for the defaults.
///
/// # Panics
/// Panics if a filter string in either constant is malformed. The strings are
/// compile-time constants, so this is a build-time invariant; the companion
/// test `default_filter_strings_are_all_parseable` proves it.
fn apply_default_filters(mut builder: ConfigStoreBuilder) -> ConfigStoreBuilder {
    for name in DEFAULT_LINT_FILTERS {
        let filter =
            LintFilter::new(AllowWarnDeny::Warn, *name).expect("DEFAULT_LINT_FILTERS entries are valid filters");
        builder = builder.with_filter(&filter);
    }
    for name in DEFAULT_ALLOWED_RULES {
        let filter =
            LintFilter::new(AllowWarnDeny::Allow, *name).expect("DEFAULT_ALLOWED_RULES entries are valid filters");
        builder = builder.with_filter(&filter);
    }
    builder
}

/// A [`ConfigStoreBuilder`] carrying oxlint's defaults, widened by
/// [`DEFAULT_LINT_FILTERS`] and narrowed by [`DEFAULT_ALLOWED_RULES`].
fn opinionated_builder() -> ConfigStoreBuilder {
    apply_default_filters(ConfigStoreBuilder::default())
}

/// Returns the lazily-initialised shared [`LintService`] configured with
/// oxlint's default rule set widened by [`DEFAULT_LINT_FILTERS`].
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
        let config = opinionated_builder()
            .build(&mut plugin_store)
            // SAFETY: the builder has no external inputs, so the build cannot fail.
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

/// Build a minimal `{"rules": {...}}` oxlint config document carrying only the
/// entries from `selection.rules` that specify tool-specific parameters (any
/// `[rules.<code>]` key other than `level`) — e.g. `[rules.max-params] max = 6`.
///
/// Each entry is written in oxlint's own native ESLint-style shape,
/// `"<code>": [<severity>, <params>]`, and handed to [`Oxlintrc::from_json_value`]
/// so that plugin/rule-name splitting, aliasing, and per-rule option-shape
/// validation all reuse oxlint's own logic rather than poly reimplementing it —
/// `OxlintRules` (the type backing `Oxlintrc::rules`) is a private type of
/// `oxc_linter`, so parsing through JSON is the *only* externally reachable way
/// to construct one.
///
/// Severity defaults to `"warn"` when the entry has no explicit `level`: naming a
/// rule under `[rules.<code>]` at all — even only to configure it — is poly's
/// signal that the rule should be active, mirroring `extend_select`. An explicit
/// `level` (handled separately, after this config is built) always wins.
///
/// Returns `None` when no entry in `selection.rules` carries params, so the
/// caller can skip the `from_oxlintrc` path entirely and fall back to
/// [`opinionated_builder`] unchanged.
fn synthesized_rules_oxlintrc(selection: &RuleSelection) -> Option<Oxlintrc> {
    let mut rules = serde_json::Map::new();
    for (code, opts) in &selection.rules {
        if opts.params.is_empty() {
            continue;
        }
        let severity = rule_options_severity_str(opts);
        let params = match serde_json::to_value(toml::Value::Table(opts.params.clone())) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(rule = %code, %error, "could not encode [rules.<id>] parameters as JSON; skipping");
                continue;
            }
        };
        rules.insert(
            code.clone(),
            serde_json::Value::Array(vec![serde_json::Value::String(severity.to_owned()), params]),
        );
    }
    if rules.is_empty() {
        return None;
    }

    let mut document = serde_json::Map::new();
    document.insert("rules".to_owned(), serde_json::Value::Object(rules));
    match Oxlintrc::from_json_value(&serde_json::Value::Object(document)) {
        Ok(oxlintrc) => Some(oxlintrc),
        Err(error) => {
            tracing::warn!(%error, "could not build synthesized oxlint config for per-rule parameters; ignoring");
            None
        }
    }
}

/// Map a [`RuleOptions::level`] to oxlint's JSON severity string, defaulting to
/// `"warn"` when unset. Mirrors the `Severity::Error => Deny, _ => Warn` mapping
/// used for the `with_filter`-based per-rule `level` override below.
fn rule_options_severity_str(opts: &RuleOptions) -> &'static str {
    match opts.level {
        Some(Severity::Error) => "error",
        _ => "warn",
    }
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
/// * `[rules.<id>] <param> = <value>` — any other key is forwarded verbatim as a
///   tool-specific option on that rule's oxlint configuration object (e.g.
///   `[rules.max-params] max = 6`). See [`synthesized_rules_oxlintrc`].
///
/// Per-rule level mapping: `"error"` → [`AllowWarnDeny::Deny`];
/// `"warning"` / `"info"` / `"hint"` → [`AllowWarnDeny::Warn`].
/// `None` level (table present, no `level` key) leaves the rule's default,
/// unless params are also present — see [`synthesized_rules_oxlintrc`].
///
/// Unrecognised or malformed rule names are silently skipped so that a typo
/// in the user's config does not prevent the other rules from running.
fn build_configured_service(cfg: &EngineConfig) -> anyhow::Result<LintService> {
    let selection = RuleSelection::from_options(cfg);

    let mut plugin_store = ExternalPluginStore::default();
    // Same opinionated base as the no-config path; user filters layer on top,
    // so an `ignore` entry can still turn any of them back off. When a rule
    // carries params, seed it from a synthesized `Oxlintrc` via `from_oxlintrc`
    // first — that is the only path that reaches `ESLintRule.config` — then layer
    // the same opinionated filters on top. `with_filter`'s `upsert_where` only
    // ever touches a rule's severity value in place when the rule is already
    // configured (`RuleEnum` equality is by rule identity, not by its baked-in
    // config), so every subsequent filter below preserves the seeded params.
    let mut builder = match synthesized_rules_oxlintrc(&selection) {
        Some(oxlintrc) => match ConfigStoreBuilder::from_oxlintrc(false, oxlintrc, None, &mut plugin_store, None) {
            Ok(builder) => apply_default_filters(builder),
            Err(error) => {
                tracing::warn!(%error, "oxlint per-rule parameters could not be applied; falling back to defaults");
                opinionated_builder()
            }
        },
        None => opinionated_builder(),
    };

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

    /// `opinionated_builder` `expect()`s on every entry of the two filter
    /// constants; this asserts the invariant directly so a bad string fails a
    /// test rather than panicking inside a rayon worker at run time.
    #[test]
    fn default_filter_strings_are_all_parseable() {
        for name in DEFAULT_LINT_FILTERS {
            assert!(
                LintFilter::new(AllowWarnDeny::Warn, *name).is_ok(),
                "DEFAULT_LINT_FILTERS entry {name:?} is not a valid oxlint filter"
            );
        }
        for name in DEFAULT_ALLOWED_RULES {
            assert!(
                LintFilter::new(AllowWarnDeny::Allow, *name).is_ok(),
                "DEFAULT_ALLOWED_RULES entry {name:?} is not a valid oxlint filter"
            );
        }
        // Cheap smoke test that the whole builder resolves against the rule
        // registry, not just that the strings parse.
        let mut plugin_store = ExternalPluginStore::default();
        assert!(opinionated_builder().build(&mut plugin_store).is_ok());
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

    /// The silent-config defect: `[rules.max-params] max = 6` (no `level` key at
    /// all) must actually reach oxlint's `max-params` rule, not just parse and get
    /// discarded. Proves the *effective threshold*, not merely "some finding
    /// changed": a 6-parameter function is exactly at the configured limit and
    /// must be clean, while a 7-parameter function must fire. oxlint's own
    /// default (`max: 3`) would flag both, so this also rules out the config
    /// having been silently ignored and the default limit applying instead.
    #[test]
    fn per_rule_params_via_rules_table_change_the_effective_threshold() {
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.max-params]
max = 6
"#,
            )
            .unwrap(),
        };

        let six_params = make_src(
            "export function six(a, b, c, d, e, f) { return a + b + c + d + e + f; }\n",
            Language::JavaScript,
        );
        let diags = lint_js(&six_params, &cfg).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("max-params")),
            "6 params is exactly the configured max; expected no max-params finding, got: {diags:#?}"
        );

        let seven_params = make_src(
            "export function seven(a, b, c, d, e, f, g) { return a + b + c + d + e + f + g; }\n",
            Language::JavaScript,
        );
        let diags = lint_js(&seven_params, &cfg).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("max-params")),
            "7 params exceeds the configured max of 6; expected a max-params finding, got: {diags:#?}"
        );
    }

    /// A four-parameter function must be clean under `max = 6` even though
    /// oxlint's own default (`max: 3`) would flag it — the regression test named
    /// in the fix's requirements, phrased directly against the reported defect.
    #[test]
    fn four_param_function_is_clean_under_configured_max_params_six() {
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.max-params]
max = 6
"#,
            )
            .unwrap(),
        };
        let src = make_src(
            "export function four(a, b, c, d) { return a + b + c + d; }\n",
            Language::JavaScript,
        );
        let diags = lint_js(&src, &cfg).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("max-params")),
            "4 params is under the configured max of 6; expected no max-params finding, got: {diags:#?}"
        );
    }

    /// Combining `level` and other params on the same `[rules.<id>]` entry: the
    /// param must still apply *and* the explicit level must win over the
    /// tool-specific-parameter default of Warn.
    #[test]
    fn per_rule_level_and_params_together_apply_both() {
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.max-params]
level = "error"
max = 6
"#,
            )
            .unwrap(),
        };
        let src = make_src(
            "export function seven(a, b, c, d, e, f, g) { return a + b + c + d + e + f + g; }\n",
            Language::JavaScript,
        );
        let diags = lint_js(&src, &cfg).unwrap();
        let d = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("max-params"))
            .expect("7 params exceeds the configured max of 6; expected a max-params finding");
        assert_eq!(
            d.severity,
            Severity::Error,
            "level = 'error' must win over the params-only default of Warn"
        );
    }

    /// The opinionated defaults (here, `no-console`) must still fire on the
    /// user-configured path, not just the no-config fast path — the per-rule
    /// `[rules.<id>]` params machinery must not replace or bypass
    /// [`opinionated_builder`]'s filters.
    #[test]
    fn opinionated_defaults_still_apply_on_the_configured_path() {
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules.max-params]
max = 6
"#,
            )
            .unwrap(),
        };
        let src = make_src("console.log(\"debug\");\n", Language::JavaScript);
        let diags = lint_js(&src, &cfg).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("no-console")),
            "no-console is one of DEFAULT_LINT_FILTERS and must still fire on the configured path; got: {diags:#?}"
        );
    }

    /// A rule referenced only via unrecognised/malformed config keys must not
    /// abort the whole lint run for the file — the file's other diagnostics
    /// (here, `no-console`) still come back.
    #[test]
    fn unknown_rule_with_params_does_not_break_the_rest_of_the_lint_run() {
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 2,
            options: toml::from_str(
                r#"
[rules."totally-not-a-real-rule"]
max = 6
"#,
            )
            .unwrap(),
        };
        let src = make_src("console.log(\"debug\");\n", Language::JavaScript);
        let diags = lint_js(&src, &cfg).unwrap();
        assert!(
            diags.iter().any(|d| d.code.as_deref() == Some("no-console")),
            "an unknown rule with params must not suppress unrelated diagnostics; got: {diags:#?}"
        );
    }
}
