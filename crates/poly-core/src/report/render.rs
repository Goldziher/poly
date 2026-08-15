//! Serialization primitives shared by the machine-readable (`json` / `toon`)
//! renderers, plus the error they report when a document cannot be produced.
//!
//! These renderers used to swallow a serializer failure and return `"[]"`. An
//! empty array is byte-identical to a clean run over zero findings, and the exit
//! code is derived from the run — never from the render — so a warning-only run
//! that failed to serialize exited `0` with an empty document: total output loss
//! presented as success. The failure is now carried to the caller, which must
//! say so and fail the run.

use serde::Serialize;

/// A machine-readable report could not be rendered.
///
/// Deliberately distinct from an empty document: emitting one on failure is the
/// defect this type exists to prevent.
#[derive(Debug)]
pub struct RenderError {
    format: &'static str,
    message: String,
}

impl RenderError {
    fn new(format: &'static str, source: impl std::fmt::Display) -> Self {
        Self {
            format,
            message: source.to_string(),
        }
    }

    /// The output format that could not be rendered — `json` or `toon`.
    pub fn format(&self) -> &'static str {
        self.format
    }
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not render the {} report: {}", self.format, self.message)
    }
}

impl std::error::Error for RenderError {}

/// Render a value as pretty-printed JSON.
pub(crate) fn render_json<T: Serialize + ?Sized>(value: &T) -> Result<String, RenderError> {
    serde_json::to_string_pretty(value).map_err(|error| RenderError::new("json", error))
}

/// Render a value as TOON, degrading to JSON if only the TOON encoder fails.
///
/// The degradation is long-standing and keeps the data rather than losing it —
/// a TOON parser handed JSON fails loudly, which is the safe direction. What is
/// *not* safe is the case below it: when the value cannot be serialized at all,
/// there is no truthful document left to print, so the original TOON failure is
/// reported instead of a clean-looking fallback.
pub(crate) fn render_toon<T: Serialize + ?Sized>(value: &T) -> Result<String, RenderError> {
    // `&value` because the TOON encoder needs a `Sized` argument; serializing a
    // reference forwards to the referent, so the encoding is unchanged.
    match serde_toon::to_string(&value) {
        Ok(text) => Ok(text),
        Err(toon_error) => render_json(value).map_err(|_| RenderError::new("toon", toon_error)),
    }
}
