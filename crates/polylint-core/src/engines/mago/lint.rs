//! PHP lint pass via [`mago_linter::Linter`].
//!
//! Two sources of diagnostics:
//! 1. **Parse errors** — `program.errors` (code `"syntax"` or `"parse"`,
//!    [`Severity::Error`]).
//! 2. **Lint issues** — [`mago_reporting::IssueCollection`] from the linter,
//!    mapped to polylint [`Diagnostic`]s.  Issues that carry exactly one safe
//!    [`mago_text_edit::TextEdit`] for the current file are wired as an
//!    [`Edit`] fix.
//!
//! ## Config keys (`[lint.php.mago]`)
//!
//! | Key | Type | Default |
//! |-----|------|---------|
//! | `select` | `[String]` | all enabled rules |
//! | `extend_select` | `[String]` | `[]` |
//! | `ignore` | `[String]` | `[]` |
//! | `[rules.<id>]` | table | — |
//! | `[rules.<id>] level` | `"error"\|"warning"\|"info"\|"hint"` | rule default |
//! | `php_version` | `"8.2"` | `"8.4"` |
//! | `integrations` | `["laravel","symfony",…]` | `[]` |
//!
//! Both rule codes (`"strict-types"`) and category names (`"correctness"`) are
//! accepted in `select`, `extend_select`, and `ignore`.  An unknown value is a
//! hard error so typos are caught early.

use std::borrow::Cow;
use std::str::FromStr as _;
use std::sync::{Arc, OnceLock};

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_database::file::HasFileId as _;
use mago_linter::Linter;
use mago_linter::integration::{Integration, IntegrationSet};
use mago_linter::registry::RuleRegistry;
use mago_linter::settings::Settings;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_reporting::Level;
use mago_span::HasSpan as _;
use mago_syntax::error::ParseError;
use mago_syntax::parser::parse_file;
use mago_text_edit::Safety;

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Edit, Severity, SourceFile, Span};
use crate::engines::rule_config::RuleSelection;

use super::rules;

/// Polylint-default PHP version used when the user does not specify one.
const PHP_VERSION: PHPVersion = PHPVersion::PHP84;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lint a single PHP source file.
///
/// `registry_cache` is the engine's [`OnceLock`]-backed registry slot.  On the
/// first call the registry is built and stored; subsequent calls reuse the
/// cached [`Arc<RuleRegistry>`] so [`RuleRegistry::build`] runs at most once
/// per engine instance (i.e. once per language per run in production).
///
/// All arena-backed objects are scoped to this function so the engine struct
/// itself stays `Send + Sync` even though [`LocalArena`] is `!Sync`.
pub(super) fn lint_php(
    src: &SourceFile,
    cfg: &EngineConfig,
    registry_cache: &OnceLock<Arc<RuleRegistry>>,
) -> anyhow::Result<Vec<Diagnostic>> {
    let arena = LocalArena::new();

    // Build an ephemeral in-memory file.
    let file = File::ephemeral(
        Cow::Borrowed(b"input.php"),
        Cow::Owned(src.content.as_bytes().to_vec()),
    );

    let program = parse_file(&arena, &file);
    let mut diags: Vec<Diagnostic> = Vec::new();

    // ── 1. Surface parse errors ──────────────────────────────────────────────
    for error in program.errors {
        let mago_span = error.span();
        let span = convert_span(mago_span, &file);
        diags.push(Diagnostic {
            engine: "mago".to_string(),
            code: Some(parse_error_code(error)),
            severity: Severity::Error,
            title: error.to_string(),
            description: None,
            span: Some(span),
            url: None,
            fix: vec![],
            metadata: Default::default(),
        });
    }

    // ── 2. Run the linter ────────────────────────────────────────────────────
    let selection = RuleSelection::from_options(cfg);
    let php_version = rules::parse_php_version(cfg)?.unwrap_or(PHP_VERSION);
    let integrations = parse_integrations(cfg)?;

    let settings = Settings {
        php_version,
        integrations,
        ..Settings::default()
    };

    // Build the 'only' allowlist when the user supplied any selection config.
    // None → run all default-enabled rules unchanged (fast path).
    let only_list: Option<Vec<String>> = if selection.is_empty() {
        None
    } else {
        Some(build_only_list(&selection, php_version, integrations)?)
    };
    let only_ref: Option<&[String]> = only_list.as_deref();

    // Retrieve or build the rule registry.
    //
    // PERF: `RuleRegistry::build` iterates all ~100 rules and compiles glob
    // patterns — measurably expensive at scale.  Because one `MagoEngine`
    // instance is created per language per run and all files share the same
    // resolved config, we cache the result in the engine's `OnceLock`.
    //
    // NOTE: The `OnceLock` initialises with the FIRST call's `settings` and
    // `only_ref`.  Since `plan_engines` creates one engine per run with a
    // constant config, all calls see consistent results.  In tests each test
    // creates its own `MagoEngine::default()`, so the lock is fresh per test.
    let registry = registry_cache
        .get_or_init(|| Arc::new(RuleRegistry::build(&settings, only_ref, false)))
        .clone();

    let names = NameResolver::new(&arena).resolve(program);
    let linter = Linter::from_registry(&arena, registry, php_version);
    let issues = linter.lint(&file, program, &names);
    let file_id = file.file_id();

    for issue in issues.iter() {
        let severity = issue_severity(issue, &selection);
        let span = issue.primary_span().map(|s| convert_span(s, &file));
        let fix = extract_safe_fixes(issue, file_id, &src.content);
        let description = issue.help.clone().or_else(|| issue.notes.first().cloned());

        diags.push(Diagnostic {
            engine: "mago".to_string(),
            code: issue.code.clone(),
            severity,
            title: issue.message.clone(),
            description,
            span,
            url: issue.link.clone(),
            fix,
            metadata: Default::default(),
        });
    }

    Ok(diags)
}

// ── Config parsing ────────────────────────────────────────────────────────────

/// Parse `integrations` from `cfg.options` as a list of integration name strings.
///
/// # Errors
///
/// Returns `anyhow::Error` when a string is not a recognised integration name.
fn parse_integrations(cfg: &EngineConfig) -> anyhow::Result<IntegrationSet> {
    let Some(arr) = cfg
        .options
        .get("integrations")
        .and_then(toml::Value::as_array)
    else {
        return Ok(IntegrationSet::empty());
    };
    let mut set = IntegrationSet::empty();
    for val in arr {
        let Some(s) = val.as_str() else {
            continue;
        };
        match Integration::from_str(s) {
            Ok(integration) => set.insert(integration),
            Err(_) => {
                anyhow::bail!(
                    "unknown mago integration name {:?}. Valid values: psl, guzzle, monolog, \
                     carbon, amphp, reactphp, symfony, laravel, tempest, neutomic, spiral, \
                     cakephp, yii, laminas, cycle, doctrine, wordpress, drupal, magento, \
                     phpunit, pest, behat, codeception, phpspec",
                    s
                );
            }
        }
    }
    Ok(set)
}

// ── Allowlist construction ────────────────────────────────────────────────────

/// Build the `only` allowlist from a [`RuleSelection`].
///
/// 1. Start with `select` (expanded) or the default-enabled codes.
/// 2. Add `extend_select` (expanded).
/// 3. Remove `ignore` (expanded).
fn build_only_list(
    selection: &RuleSelection,
    php_version: PHPVersion,
    integrations: IntegrationSet,
) -> anyhow::Result<Vec<String>> {
    // Base set.
    let mut active: Vec<String> = if selection.select.is_empty() {
        rules::default_enabled_codes(php_version, integrations)
    } else {
        rules::expand_to_codes(&selection.select, php_version, integrations)?
    };

    // extend_select — adds codes not already present.
    let extended = rules::expand_to_codes(&selection.extend_select, php_version, integrations)?;
    for code in extended {
        if !active.contains(&code) {
            active.push(code);
        }
    }

    // ignore — removes matching codes.
    let ignored = rules::expand_to_codes(&selection.ignore, php_version, integrations)?;
    active.retain(|code| !ignored.contains(code));

    Ok(active)
}

// ── Severity helpers ──────────────────────────────────────────────────────────

/// Determine the polylint [`Severity`] for a lint issue, applying any
/// per-rule level override from `selection.rules`.
fn issue_severity(issue: &mago_reporting::Issue, selection: &RuleSelection) -> Severity {
    // Check for a user-configured level override first.
    if let Some(code) = issue.code.as_deref()
        && let Some(opts) = selection.rules.get(code)
        && let Some(level) = opts.level
    {
        return level;
    }
    map_level(issue.level)
}

/// Convert a mago [`Level`] to a polylint [`Severity`].
fn map_level(level: Level) -> Severity {
    match level {
        Level::Error => Severity::Error,
        Level::Warning => Severity::Warning,
        Level::Help => Severity::Hint,
        Level::Note => Severity::Info,
    }
}

// ── Span / edit helpers ───────────────────────────────────────────────────────

/// Convert a mago byte-offset [`mago_span::Span`] to a polylint 1-based
/// line/column [`Span`] using [`File`]'s built-in line-number index.
fn convert_span(span: mago_span::Span, file: &File) -> Span {
    let start_line_0 = file.line_number(span.start.offset);
    let end_line_0 = file.line_number(span.end.offset);

    let start_line_byte = file.get_line_start_offset(start_line_0).unwrap_or(0);
    let end_line_byte = file.get_line_start_offset(end_line_0).unwrap_or(0);

    Span {
        start_line: start_line_0 + 1,
        start_col: (span.start.offset.saturating_sub(start_line_byte)) + 1,
        end_line: end_line_0 + 1,
        end_col: (span.end.offset.saturating_sub(end_line_byte)) + 1,
    }
}

/// Return a short stable code string for a parse error kind.
fn parse_error_code(error: &ParseError) -> String {
    match error {
        ParseError::SyntaxError(_) | ParseError::UnclosedLiteralString(_, _) => {
            "syntax".to_string()
        }
        _ => "parse".to_string(),
    }
}

/// Extract all safe [`Edit`]s from a lint issue that apply to `file_id`.
///
/// Only edits marked [`Safety::Safe`] within the byte bounds of `source` are
/// included.  The runner applies the returned set atomically (with an internal
/// overlap guard), so it is safe to return multiple edits.
fn extract_safe_fixes(
    issue: &mago_reporting::Issue,
    file_id: mago_database::file::FileId,
    source: &str,
) -> Vec<Edit> {
    let Some(edits) = issue.edits.get(&file_id) else {
        return vec![];
    };
    edits
        .iter()
        .filter(|e| e.safety == Safety::Safe)
        .filter_map(|e| {
            let start = e.range.start as usize;
            let end = e.range.end as usize;
            if end > source.len() || start > end {
                return None;
            }
            let replacement = String::from_utf8(e.new_text.clone()).ok()?;
            Some(Edit {
                start_byte: start,
                end_byte: end,
                replacement,
            })
        })
        .collect()
}
