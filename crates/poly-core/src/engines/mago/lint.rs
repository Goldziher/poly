//! PHP lint pass via [`mago_linter::Linter`].
//!
//! Two sources of diagnostics:
//! 1. **Parse errors** — `program.errors` (code `"syntax"` or `"parse"`,
//!    [`Severity::Error`]).
//! 2. **Lint issues** — [`mago_reporting::IssueCollection`] from the linter,
//!    mapped to poly [`Diagnostic`]s.  Issues that carry exactly one safe
//!    [`mago_text_edit::TextEdit`] for the current file are wired as an
//!    [`Edit`] fix.
//!
//! ## Severity
//!
//! Mago's [`Level`] maps straight through, except that the `Maintainability`
//! metric rules listed in [`ADVISORY_RULE_CODES`] are downgraded from `Error`
//! to [`Severity::Warning`] so a complexity budget never fails a run.  See that
//! constant for the rationale and the escape hatch.
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

    let file = File::ephemeral(Cow::Borrowed(b"input.php"), Cow::Owned(src.content.as_bytes().to_vec()));

    let program = parse_file(&arena, &file);
    let mut diags: Vec<Diagnostic> = Vec::new();

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

    let selection = RuleSelection::from_options(cfg);
    let php_version = rules::parse_php_version(cfg)?.unwrap_or(PHP_VERSION);
    let integrations = parse_integrations(cfg)?;

    let settings = Settings {
        php_version,
        integrations,
        ..Settings::default()
    };

    let only_list: Option<Vec<String>> = if selection.is_empty() {
        None
    } else {
        Some(build_only_list(&selection, php_version, integrations)?)
    };
    let only_ref: Option<&[String]> = only_list.as_deref();

    // NOTE: The `OnceLock` initialises with the FIRST call's `settings` and
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

/// Parse `integrations` from `cfg.options` as a list of integration name strings.
///
/// # Errors
///
/// Returns `anyhow::Error` when a string is not a recognised integration name.
fn parse_integrations(cfg: &EngineConfig) -> anyhow::Result<IntegrationSet> {
    let Some(arr) = cfg.options.get("integrations").and_then(toml::Value::as_array) else {
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
    let mut active: Vec<String> = if selection.select.is_empty() {
        rules::default_enabled_codes(php_version, integrations)
    } else {
        rules::expand_to_codes(&selection.select, php_version, integrations)?
    };

    let extended = rules::expand_to_codes(&selection.extend_select, php_version, integrations)?;
    for code in extended {
        if !active.contains(&code) {
            active.push(code);
        }
    }

    let ignored = rules::expand_to_codes(&selection.ignore, php_version, integrations)?;
    active.retain(|code| !ignored.contains(code));

    Ok(active)
}

/// Rule codes reported at [`Severity::Warning`] rather than mago's own `Error`,
/// so they never fail a run.
///
/// `poly lint` exits non-zero only on error-severity findings, so the severity a
/// backend assigns decides whether a rule is a **gate** or a **guard rail**. Mago
/// ships six default-enabled rules whose `Config::default()` is `Level::Error`
/// but which measure a *metric* rather than detect a defect — all six live in
/// [`mago_linter::category::Category::Maintainability`]. Passing them through
/// verbatim means PHP is the one language where poly fails CI on a complexity
/// budget, contradicting ADR 0027's "warning severity, on by default" posture.
///
/// The split follows what a finding *means*, not what mago calls it:
///
/// - **Error (not listed here)** — the other 21 default-enabled `Level::Error`
///   rules: `Safety` (`no-eval`, `no-ffi`, `no-unsafe-finally`, …), `Security`
///   (`no-literal-password`, `tainted-data-to-sink`, `sensitive-parameter`, …),
///   `Correctness`, `Deprecation`, `BestPractices`, and `Clarity`'s `no-empty`.
///   These report a real correctness or security problem, so they stay gates.
///   `no-empty` is the closest call — an empty `catch` swallows an error — but
///   it is defect detection rather than a metric, so it keeps `Error`.
/// - **Warning (listed here)** — the `Maintainability` metric thresholds. Each
///   is a threshold poly does not endorse as a build breaker: complexity 15,
///   5 parameters, 10 methods, 10 properties, 20 enum cases, Kan defect 1.
///
/// A consumer opts back into failing on any of them with
/// `[lint.php.mago.rules.<code>] level = "error"`, which [`issue_severity`]
/// applies *before* this downgrade.
///
/// Verified against mago-linter 1.47.3: `src/rule/maintainability/`
/// `cyclomatic_complexity.rs`, `excessive_parameter_list.rs`, `kan_defect.rs`,
/// `too_many_enum_cases.rs`, `too_many_methods.rs`, `too_many_properties.rs`
/// each declare `level: Level::Error` in their `impl Default for …Config`.
static ADVISORY_RULE_CODES: &[&str] = &[
    "cyclomatic-complexity",
    "excessive-parameter-list",
    "kan-defect",
    "too-many-enum-cases",
    "too-many-methods",
    "too-many-properties",
];

/// Downgrade `severity` to [`Severity::Warning`] when `code` is one of the
/// advisory maintainability metrics (see [`ADVISORY_RULE_CODES`]).
///
/// Only ever downgrades: a finding mago already reports below `Error` keeps its
/// own level.
fn advisory_severity(code: Option<&str>, severity: Severity) -> Severity {
    if severity != Severity::Error {
        return severity;
    }
    match code {
        Some(code) if ADVISORY_RULE_CODES.contains(&code) => Severity::Warning,
        _ => severity,
    }
}

/// Determine the poly [`Severity`] for a lint issue, applying any
/// per-rule level override from `selection.rules`.
///
/// The user override is consulted first and returns early, so
/// `[rules.cyclomatic-complexity] level = "error"` still promotes an advisory
/// rule back to a gate.
fn issue_severity(issue: &mago_reporting::Issue, selection: &RuleSelection) -> Severity {
    let code = issue.code.as_deref();
    if let Some(code) = code
        && let Some(opts) = selection.rules.get(code)
        && let Some(level) = opts.level
    {
        return level;
    }
    advisory_severity(code, map_level(issue.level))
}

/// Convert a mago [`Level`] to a poly [`Severity`].
fn map_level(level: Level) -> Severity {
    match level {
        Level::Error => Severity::Error,
        Level::Warning => Severity::Warning,
        Level::Help => Severity::Hint,
        Level::Note => Severity::Info,
    }
}

/// Convert a mago byte-offset [`mago_span::Span`] to a poly 1-based
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
        ParseError::SyntaxError(_) | ParseError::UnclosedLiteralString(_, _) => "syntax".to_string(),
        _ => "parse".to_string(),
    }
}

/// Extract all safe [`Edit`]s from a lint issue that apply to `file_id`.
///
/// Only edits marked [`Safety::Safe`] within the byte bounds of `source` are
/// included.  The runner applies the returned set atomically (with an internal
/// overlap guard), so it is safe to return multiple edits.
fn extract_safe_fixes(issue: &mago_reporting::Issue, file_id: mago_database::file::FileId, source: &str) -> Vec<Edit> {
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
