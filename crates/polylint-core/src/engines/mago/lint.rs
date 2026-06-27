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
//! PHP version defaults to PHP 8.4; the linter settings default is PHP 8.0 but
//! we override it to match the formatter.

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_database::file::HasFileId as _;
use mago_linter::Linter;
use mago_linter::settings::Settings;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_reporting::Level;
use mago_span::HasSpan as _;
use mago_syntax::parser::parse_file;
use mago_text_edit::Safety;

use crate::config::EngineConfig;
use crate::engine::{Diagnostic, Edit, Severity, SourceFile, Span};

/// Target PHP version for linting and name resolution.
const PHP_VERSION: PHPVersion = PHPVersion::PHP84;

/// Lint a single PHP source file.
///
/// All arena-backed objects are scoped to this function so the engine struct
/// itself stays `Send + Sync` even though [`LocalArena`] is `!Sync`.
pub(super) fn lint_php(src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
    let arena = LocalArena::new();

    // Build an ephemeral in-memory file.  The name must be `Cow<'static, [u8]>`;
    // the contents require an owned copy because File takes ownership.
    let file = File::ephemeral(
        Cow::Borrowed(b"input.php"),
        Cow::Owned(src.content.as_bytes().to_vec()),
    );

    // Parse the file.  The program (and its errors slice) borrows from `arena`.
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
            message: error.to_string(),
            span: Some(span),
            fix: None,
            metadata: Default::default(),
        });
    }

    // ── 2. Run the linter ────────────────────────────────────────────────────
    let settings = Settings {
        php_version: PHP_VERSION,
        ..Settings::default()
    };
    let names = NameResolver::new(&arena).resolve(program);
    let linter = Linter::new(&arena, &settings, None, false);
    let issues = linter.lint(&file, program, &names);

    let file_id = file.file_id();

    for issue in issues.iter() {
        let severity = match issue.level {
            Level::Error => Severity::Error,
            Level::Warning => Severity::Warning,
            Level::Help => Severity::Hint,
            Level::Note => Severity::Info,
        };

        // Use the primary annotation span as the diagnostic location.
        let span = issue.primary_span().map(|s| convert_span(s, &file));

        // Wire a fix only when the issue has exactly one safe edit on the
        // current file — matches how ruff.rs attaches fixes conservatively.
        let fix = extract_single_safe_fix(issue, file_id, &src.content);

        diags.push(Diagnostic {
            engine: "mago".to_string(),
            code: issue.code.clone(),
            severity,
            message: issue.message.clone(),
            span,
            fix,
            metadata: Default::default(),
        });
    }

    Ok(diags)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

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
fn parse_error_code(error: &mago_syntax::error::ParseError) -> String {
    use mago_syntax::error::ParseError;
    match error {
        ParseError::SyntaxError(_) => "syntax".to_string(),
        ParseError::UnclosedLiteralString(_, _) => "syntax".to_string(),
        _ => "parse".to_string(),
    }
}

/// Extract a single safe [`Edit`] fix from a lint issue, or `None` if the
/// issue has zero, multiple, or only unsafe edits.
fn extract_single_safe_fix(
    issue: &mago_reporting::Issue,
    file_id: mago_database::file::FileId,
    source: &str,
) -> Option<Edit> {
    // Only wire fixes when the issue affects exactly one file (the current
    // file) and that file has exactly one edit.
    if issue.edits.len() != 1 {
        return None;
    }
    let edits = issue.edits.get(&file_id)?;
    if edits.len() != 1 {
        return None;
    }
    let edit = &edits[0];
    if edit.safety != Safety::Safe {
        return None;
    }
    // Validate byte range is within source bounds.
    let start = edit.range.start as usize;
    let end = edit.range.end as usize;
    if end > source.len() || start > end {
        return None;
    }
    let replacement = String::from_utf8(edit.new_text.clone()).ok()?;
    Some(Edit {
        start_byte: start,
        end_byte: end,
        replacement,
    })
}
