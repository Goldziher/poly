//! dotenv linter backend, wrapping `dotenv-analyzer` + `dotenv-core`.
//!
//! `dotenv-analyzer` is the library extracted from the `dotenv-linter` CLI: it
//! runs a fixed list of per-line (and whole-file) checks over a
//! `Vec<dotenv_core::LineEntry>` and can produce a corrected copy of that
//! vector via [`fix`]. This backend adapts poly's `SourceFile`/`Diagnostic`/
//! `Edit` model onto that API.
//!
//! # Free win: inline suppression comments
//!
//! `dotenv-analyzer::check` already understands `# dotenv-linter:off <Rule>`
//! / `# dotenv-linter:on <Rule>` control comments (see `dotenv_analyzer`'s
//! `Comment` parser) and honors them while scanning — poly does not need to
//! implement its own suppression-comment handling for this backend.
//!
//! # Autofix reports one edit, not N
//!
//! Autofix semantics for this backend: when any diagnostic on a file
//! carries a fix, exactly **one** whole-file [`Edit`] is attached, to the
//! *first* diagnostic only — every other diagnostic on the same lint pass
//! carries no edit. The runner's autofix loop applies edits diagnostic by
//! diagnostic and would otherwise apply N overlapping whole-file rewrites
//! computed against the *original* content, silently discarding all but the
//! last one applied. Producing a single corrected copy of the file (via one
//! [`fix`] call over every warning) and attaching it once is correct; the
//! consequence is that a fixed file's `LintResult.fixed` counter reports `1`
//! even when `fix()` corrected many separate warnings in that same file. This
//! is a real, inherent limitation of the "N diagnostics, 1 combined edit"
//! shape — not a bug — and callers that need an exact per-warning fixed count
//! must not read it from this backend's `LintResult.fixed`.

use std::str::FromStr;

use dotenv_analyzer::{LintKind, Warning, check, fix};
use dotenv_core::LineEntry;

use super::rule_config::RuleSelection;
use crate::config::EngineConfig;
use crate::engine::{Capabilities, Diagnostic, Edit, Engine, Severity, SourceFile, Span};
use crate::language::Language;

/// Cache-key version: `dotenv-analyzer` + `dotenv-core` crate versions, plus a
/// marker for this backend's own line-entry construction / rendering logic.
/// Bump whenever either crate is updated OR the mapping logic changes.
const DOTENV_VERSION: &str = "dotenv-analyzer-0.1.1+core-0.1.1+map1";

/// Every [`LintKind`] variant, used to translate a `select`/`ignore` rule
/// selection (an inclusion/exclusion vocabulary) into the crate's own
/// `skip_checks` exclusion list.
const ALL_LINT_KINDS: &[LintKind] = &[
    LintKind::DuplicatedKey,
    LintKind::EndingBlankLine,
    LintKind::ExtraBlankLine,
    LintKind::IncorrectDelimiter,
    LintKind::KeyWithoutValue,
    LintKind::LeadingCharacter,
    LintKind::LowercaseKey,
    LintKind::QuoteCharacter,
    LintKind::SpaceCharacter,
    LintKind::SubstitutionKey,
    LintKind::TrailingWhitespace,
    LintKind::UnorderedKey,
    LintKind::ValueWithoutQuotes,
    LintKind::SchemaViolation,
];

static LANGUAGES: &[Language] = &[Language::Dotenv];

/// dotenv (`.env`) linter backend. See the module docs.
pub struct DotenvEngine;

impl Engine for DotenvEngine {
    fn name(&self) -> &'static str {
        "dotenv"
    }

    fn languages(&self) -> &'static [Language] {
        LANGUAGES
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: true,
        }
    }

    fn version(&self) -> &str {
        DOTENV_VERSION
    }

    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        if src.content.is_empty() {
            return Ok(Vec::new());
        }

        let selection = RuleSelection::from_options(cfg);
        let skip_checks = skip_checks_from_selection(&selection);

        let entries = build_line_entries(&src.content);
        let warnings = check(&entries, &skip_checks, None);
        if warnings.is_empty() {
            return Ok(Vec::new());
        }

        let mut fixed_entries = entries.clone();
        let fixed_count = fix(&warnings, &mut fixed_entries, &skip_checks);

        let mut diags: Vec<Diagnostic> = warnings
            .into_iter()
            .map(|warning| warning_to_diagnostic(&entries, &selection, warning))
            .collect();

        // See the module docs: a single combined edit, attached to the first
        // diagnostic only, never one edit per warning.
        if fixed_count > 0
            && let Some(first) = diags.first_mut()
        {
            let replacement = render_line_entries(&fixed_entries);
            if replacement != *src.content {
                first.fix.push(Edit {
                    start_byte: 0,
                    end_byte: src.content.len(),
                    replacement,
                });
            }
        }

        Ok(diags)
    }
}

/// Translate a `[lint.dotenv]` `select`/`extend_select`/`ignore` selection
/// into the `skip_checks` exclusion list `check()`/`fix()` expect.
///
/// `select`/`extend_select` name the checks to *keep*; `ignore` names checks
/// to drop even from a kept set. An empty selection keeps every check (the
/// crate's own default), matching `RuleSelection::is_empty`.
fn skip_checks_from_selection(selection: &RuleSelection) -> Vec<LintKind> {
    if selection.is_empty() {
        return Vec::new();
    }

    let mut keep: Vec<LintKind> = if selection.select.is_empty() {
        ALL_LINT_KINDS.to_vec()
    } else {
        selection
            .select
            .iter()
            .filter_map(|code| LintKind::from_str(code).ok())
            .collect()
    };
    for code in &selection.extend_select {
        if let Ok(kind) = LintKind::from_str(code)
            && !keep.contains(&kind)
        {
            keep.push(kind);
        }
    }

    let ignore: Vec<LintKind> = selection
        .ignore
        .iter()
        .filter_map(|code| LintKind::from_str(code).ok())
        .collect();
    ALL_LINT_KINDS
        .iter()
        .copied()
        .filter(|kind| !keep.contains(kind) || ignore.contains(kind))
        .collect()
}

/// Build the `Vec<LineEntry>` `dotenv-analyzer` operates over.
///
/// Mirrors the real `dotenv-linter` CLI's file-to-`LineEntry` construction
/// (`dotenv-finder`'s `FileEntry::from`): `str::lines()` strips every
/// terminator, which loses the fact that a properly-ended file has a final
/// blank line — [`EndingBlankLineChecker`](dotenv_analyzer) reads that
/// directly off the last entry's `raw_string`. So when `content` ends with
/// `\n`, one extra entry whose `raw_string` is exactly `"\n"` is appended
/// after the real lines, marked as the (new) last line; [`render_line_entries`]
/// reverses this exactly.
fn build_line_entries(content: &str) -> Vec<LineEntry> {
    let mut raw_lines: Vec<String> = content.lines().map(str::to_owned).collect();
    if content.ends_with('\n') {
        raw_lines.push("\n".to_owned());
    }
    let total = raw_lines.len();
    raw_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| LineEntry::new(index + 1, line, index + 1 == total))
        .collect()
}

/// Reverse [`build_line_entries`]: join line text back into a single string.
///
/// A real content line's `raw_string` never contains `\n` (see
/// `build_line_entries`), so the trailing synthetic `"\n"` marker entry, when
/// present, must contribute exactly one final newline rather than being
/// joined in like an ordinary line (which would double it up).
fn render_line_entries(entries: &[LineEntry]) -> String {
    match entries.split_last() {
        Some((last, rest)) if last.raw_string == "\n" => {
            let mut body: String = rest
                .iter()
                .map(|entry| entry.raw_string.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            body.push('\n');
            body
        }
        _ => entries
            .iter()
            .map(|entry| entry.raw_string.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Map one `dotenv_analyzer::Warning` to a normalized [`Diagnostic`].
///
/// `Warning` carries no column information, so the span covers the whole
/// line: `start_col = 1` to `end_col = <line length> + 1`.
fn warning_to_diagnostic(entries: &[LineEntry], selection: &RuleSelection, warning: Warning) -> Diagnostic {
    let code = warning.check_name().to_string();
    let severity = selection
        .rules
        .get(&code)
        .and_then(|options| options.level)
        .unwrap_or(Severity::Warning);

    let line_number = warning.line_number();
    let line_len = entries
        .iter()
        .find(|entry| entry.number == line_number)
        .map(|entry| {
            if entry.raw_string == "\n" {
                0
            } else {
                entry.raw_string.len() as u32
            }
        })
        .unwrap_or(0);
    let line = line_number as u32;

    Diagnostic {
        engine: "dotenv".to_owned(),
        code: Some(code),
        severity,
        title: warning.message().to_owned(),
        description: None,
        span: Some(Span {
            start_line: line,
            start_col: 1,
            end_line: line,
            end_col: line_len.saturating_add(1),
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use crate::config::GlobalDefaults;

    use super::*;

    fn engine_cfg() -> EngineConfig {
        EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::Table::new(),
        }
    }

    fn make_src(content: &str) -> SourceFile {
        SourceFile {
            path: ".env".into(),
            language: Language::Dotenv,
            content: content.into(),
        }
    }

    #[test]
    fn build_line_entries_appends_synthetic_blank_line_for_trailing_newline() {
        let entries = build_line_entries("FOO=BAR\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].raw_string, "FOO=BAR");
        assert!(!entries[0].is_last_line);
        assert_eq!(entries[1].raw_string, "\n");
        assert!(entries[1].is_last_line);
    }

    #[test]
    fn build_line_entries_no_synthetic_line_without_trailing_newline() {
        let entries = build_line_entries("FOO=BAR");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].raw_string, "FOO=BAR");
        assert!(entries[0].is_last_line);
    }

    #[test]
    fn render_line_entries_round_trips_trailing_newline() {
        let content = "FOO=BAR\nBAZ=QUX\n";
        let entries = build_line_entries(content);
        assert_eq!(render_line_entries(&entries), content);
    }

    #[test]
    fn render_line_entries_round_trips_no_trailing_newline() {
        let content = "FOO=BAR\nBAZ=QUX";
        let entries = build_line_entries(content);
        assert_eq!(render_line_entries(&entries), content);
    }

    #[test]
    fn clean_file_has_no_diagnostics() {
        let engine = DotenvEngine;
        let src = make_src("BAZ=QUX\nFOO=BAR\n");
        let diags = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(diags.is_empty(), "expected no diagnostics for a clean file: {diags:?}");
    }

    #[test]
    fn ignore_suppresses_selected_rule() {
        let engine = DotenvEngine;
        let src = make_src("foo=bar\n");
        let baseline = engine.lint(&src, &engine_cfg()).unwrap();
        assert!(
            baseline.iter().any(|d| d.code.as_deref() == Some("LowercaseKey")),
            "expected LowercaseKey to fire on a lowercase key: {baseline:?}"
        );

        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(r#"ignore = ["LowercaseKey"]"#).unwrap(),
        };
        let diags = engine.lint(&src, &cfg).unwrap();
        assert!(
            !diags.iter().any(|d| d.code.as_deref() == Some("LowercaseKey")),
            "ignore must suppress LowercaseKey: {diags:?}"
        );
    }

    #[test]
    fn rule_level_override_changes_severity() {
        let engine = DotenvEngine;
        let src = make_src("foo=bar\n");
        let cfg = EngineConfig {
            globals: GlobalDefaults::default(),
            indent_width: 4,
            options: toml::from_str(
                r#"
[rules.LowercaseKey]
level = "error"
"#,
            )
            .unwrap(),
        };
        let diags = engine.lint(&src, &cfg).unwrap();
        let diag = diags
            .iter()
            .find(|d| d.code.as_deref() == Some("LowercaseKey"))
            .expect("LowercaseKey diagnostic");
        assert_eq!(diag.severity, Severity::Error);
    }
}
