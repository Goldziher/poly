//! ty diagnostics as poly [`Diagnostic`]s.

use poly_engine_api::engine::{Diagnostic, Severity, Span};
use ruff_db::Db as SourceDb;
use ruff_db::diagnostic::{Diagnostic as TyDiagnostic, Severity as TySeverity, UnifiedFile};
use ruff_db::files::File;
use ruff_db::source::{line_index, source_text};

use super::ENGINE_NAME;

fn severity(severity: TySeverity) -> Severity {
    match severity {
        TySeverity::Fatal | TySeverity::Error => Severity::Error,
        TySeverity::Warning => Severity::Warning,
        TySeverity::Info => Severity::Info,
        TySeverity::Hint => Severity::Hint,
    }
}

/// The file a diagnostic is anchored in and its poly form; `None` for one with no file of its own.
pub(super) fn convert(db: &dyn SourceDb, diagnostic: &TyDiagnostic) -> Option<(File, Diagnostic)> {
    let primary = diagnostic.primary_span()?;
    let UnifiedFile::Ty(file) = primary.file() else {
        return None;
    };
    let file = *file;
    let span = primary.range().map(|range| {
        let text = source_text(db, file);
        let index = line_index(db, file);
        let start = index.line_column(range.start(), text.as_str());
        let end = index.line_column(range.end(), text.as_str());
        Span {
            start_line: start.line.get() as u32,
            start_col: start.column.get() as u32,
            end_line: end.line.get() as u32,
            end_col: end.column.get() as u32,
        }
    });
    Some((
        file,
        Diagnostic {
            engine: ENGINE_NAME.to_string(),
            code: Some(diagnostic.id().as_str().to_string()),
            severity: severity(diagnostic.severity()),
            title: diagnostic.concise_message().to_string(),
            description: diagnostic.first_help_text().map(str::to_string),
            span,
            url: None,
            fix: Vec::new(),
            metadata: Default::default(),
        },
    ))
}
