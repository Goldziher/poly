//! Mapping engine failures onto [`WireError`].

use std::panic::{AssertUnwindSafe, catch_unwind};

use poly_component_protocol::{WireError, WireErrorKind};
use poly_engine_api::engine_guard::panic_message;

/// A protocol violation (malformed header, bad slice bounds, invalid UTF-8, bad config).
pub(crate) fn protocol(message: impl Into<String>) -> WireError {
    WireError {
        kind: WireErrorKind::Protocol,
        message: message.into(),
        chain: Vec::new(),
    }
}

/// An engine error with its full cause chain, outermost cause first.
pub(crate) fn from_anyhow(error: &anyhow::Error) -> WireError {
    WireError {
        kind: WireErrorKind::Error,
        message: error.to_string(),
        chain: error.chain().skip(1).map(ToString::to_string).collect(),
    }
}

/// Run `operation`, converting a panic into [`WireErrorKind::Panic`] with the wording of poly's
/// engine guard.
pub(crate) fn guarded<T>(engine: &str, operation: impl FnOnce() -> anyhow::Result<T>) -> Result<T, WireError> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(from_anyhow(&error)),
        Err(payload) => {
            let detail = panic_message(payload.as_ref());
            tracing::warn!(
                engine = %engine,
                detail = %detail,
                "backend panicked; the file was not checked. Please report this upstream"
            );
            Err(WireError {
                kind: WireErrorKind::Panic,
                message: format!("the '{engine}' backend panicked on this file: {detail}"),
                chain: Vec::new(),
            })
        }
    }
}
