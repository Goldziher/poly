//! Conversions between the engine report types and the component wire DTOs.
//!
//! The wire types carry every field explicitly (no `skip_serializing_if`), so a round trip through
//! [`WireDiagnostic`] is lossless.

mod config;

use poly_component_protocol::{WireDiagnostic, WireEdit, WireSeverity, WireSpan};

pub use config::{engine_config_from_wire, engine_config_to_wire};

use crate::engine::{Diagnostic, Edit, Severity, Span};

impl From<Severity> for WireSeverity {
    fn from(severity: Severity) -> Self {
        match severity {
            Severity::Error => WireSeverity::Error,
            Severity::Warning => WireSeverity::Warning,
            Severity::Info => WireSeverity::Info,
            Severity::Hint => WireSeverity::Hint,
        }
    }
}

impl From<WireSeverity> for Severity {
    fn from(severity: WireSeverity) -> Self {
        match severity {
            WireSeverity::Error => Severity::Error,
            WireSeverity::Warning => Severity::Warning,
            WireSeverity::Info => Severity::Info,
            WireSeverity::Hint => Severity::Hint,
        }
    }
}

impl From<Span> for WireSpan {
    fn from(span: Span) -> Self {
        WireSpan {
            start_line: span.start_line,
            start_col: span.start_col,
            end_line: span.end_line,
            end_col: span.end_col,
        }
    }
}

impl From<WireSpan> for Span {
    fn from(span: WireSpan) -> Self {
        Span {
            start_line: span.start_line,
            start_col: span.start_col,
            end_line: span.end_line,
            end_col: span.end_col,
        }
    }
}

impl From<&Edit> for WireEdit {
    fn from(edit: &Edit) -> Self {
        WireEdit {
            start_byte: edit.start_byte as u64,
            end_byte: edit.end_byte as u64,
            replacement: edit.replacement.clone(),
        }
    }
}

/// A wire offset that does not fit this platform's `usize` (a 32-bit host given a >4 GiB offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetOutOfRange(pub u64);

impl std::fmt::Display for OffsetOutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "byte offset {} does not fit in usize", self.0)
    }
}

impl std::error::Error for OffsetOutOfRange {}

impl TryFrom<WireEdit> for Edit {
    type Error = OffsetOutOfRange;

    fn try_from(edit: WireEdit) -> Result<Self, Self::Error> {
        let offset = |v: u64| usize::try_from(v).map_err(|_| OffsetOutOfRange(v));
        Ok(Edit {
            start_byte: offset(edit.start_byte)?,
            end_byte: offset(edit.end_byte)?,
            replacement: edit.replacement,
        })
    }
}

impl From<&Diagnostic> for WireDiagnostic {
    fn from(d: &Diagnostic) -> Self {
        WireDiagnostic {
            engine: d.engine.clone(),
            code: d.code.clone(),
            severity: d.severity.into(),
            title: d.title.clone(),
            description: d.description.clone(),
            span: d.span.map(Into::into),
            url: d.url.clone(),
            fix: d.fix.iter().map(WireEdit::from).collect(),
            metadata: d.metadata.clone(),
        }
    }
}

impl TryFrom<WireDiagnostic> for Diagnostic {
    type Error = OffsetOutOfRange;

    fn try_from(d: WireDiagnostic) -> Result<Self, Self::Error> {
        Ok(Diagnostic {
            engine: d.engine,
            code: d.code,
            severity: d.severity.into(),
            title: d.title,
            description: d.description,
            span: d.span.map(Into::into),
            url: d.url,
            fix: d.fix.into_iter().map(Edit::try_from).collect::<Result<_, _>>()?,
            metadata: d.metadata,
        })
    }
}
