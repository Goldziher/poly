//! Containment for panics raised inside a wrapped upstream parser.
//!
//! poly compiles roughly twenty third-party parsers in-process. Their public
//! APIs return `Result`, but their internals `assert!`: `biome_parser` asserts
//! on a GraphQL *fragment description* — a real language feature
//! (graphql-js #4482) — and sqruff's `FluffConfig::from_source` panics rather
//! than errors on a malformed document. poly does not control that code and
//! cannot audit every edge case in it.
//!
//! Without containment a single such file is fatal to the **entire run**,
//! because engines are invoked inside a rayon `par_iter` and rayon propagates
//! a worker panic to the caller. Measured on prettier's test corpus: `poly
//! lint` over a 63 MB repository produced a **0-byte** report and exited 101,
//! caused by one three-line `.graphql` file. Not a partial report, not an exit
//! code meaning "some files failed" — nothing at all.
//!
//! That outcome is strictly worse than the one poly already models. A file the
//! engine cannot process is an ordinary per-file `error`, counted, reported,
//! and answered with exit code 2 ("the run verified less than it claims").
//! This module routes a panic into that existing path so the other 20,000
//! files in the repository still get checked.
//!
//! # What this does not do
//!
//! - It does not install a panic hook. `poly-core` is a library — `poly mcp`
//!   and the CLI both link it — and a library that mutates process-global panic
//!   state surprises its host. The default hook still prints, so a panicking
//!   parser stays loud; it simply stops being fatal.
//! - It does not make the engine's own state safe to reuse. The guard is
//!   applied per file at the call site, and an `Engine` is a stateless
//!   dispatcher over borrowed input, so a panic cannot leave a half-updated
//!   engine visible to the next file.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

/// Run `operation`, converting a panic into an `Err` that names the engine and
/// the file.
///
/// The panic message is recovered from the payload when it is a `String` or
/// `&str` — which covers `panic!`, `assert!`, `unwrap` and `expect` — and
/// falls back to a fixed description otherwise, so the caller always gets an
/// actionable error rather than an opaque one.
///
/// [`AssertUnwindSafe`] is sound here because nothing observable outlives the
/// call: `operation` borrows the source text and the engine config immutably
/// and returns an owned result, so a panic midway cannot leave a caller-visible
/// value in a torn state.
pub(crate) fn guard_engine_panic<T>(
    engine: &str,
    path: &Path,
    operation: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(result) => result,
        Err(payload) => {
            let detail = panic_message(payload.as_ref());
            tracing::warn!(
                engine = %engine,
                file = %path.display(),
                detail = %detail,
                "backend panicked; the file was not checked. Please report this upstream"
            );
            Err(anyhow::anyhow!(
                "the '{engine}' backend panicked on this file: {detail}"
            ))
        }
    }
}

/// Best-effort recovery of a panic payload's message.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else {
        "panic payload was not a string".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_operation_passes_its_value_through() {
        let value = guard_engine_panic("test", Path::new("f.rs"), || Ok(7)).expect("no panic");
        assert_eq!(value, 7);
    }

    #[test]
    fn an_ordinary_error_is_not_rewritten_as_a_panic() {
        let error = guard_engine_panic::<()>("test", Path::new("f.rs"), || Err(anyhow::anyhow!("plain failure")))
            .expect_err("the error is propagated");
        assert!(
            error.to_string().contains("plain failure"),
            "an Err must survive unchanged, got {error}"
        );
        assert!(
            !error.to_string().contains("panicked"),
            "an Err must not be relabelled as a panic, got {error}"
        );
    }

    /// Both payload shapes a real backend produces: `panic!("...")` with
    /// formatting yields a `String`, a bare `panic!("literal")` yields a
    /// `&'static str`, and `assert_eq!` yields a `String`.
    #[test]
    fn a_panic_becomes_an_error_naming_the_engine_and_the_message() {
        let formatted = guard_engine_panic::<()>("biome", Path::new("a.graphql"), || {
            panic!("expected {} but found {}", "FRAGMENT_KW", "STRING")
        })
        .expect_err("a panic is converted to an error");
        assert!(formatted.to_string().contains("biome"), "{formatted}");
        assert!(
            formatted.to_string().contains("expected FRAGMENT_KW but found STRING"),
            "{formatted}"
        );

        let literal = guard_engine_panic::<()>("sqruff", Path::new("a.sql"), || panic!("malformed document"))
            .expect_err("a panic is converted to an error");
        assert!(literal.to_string().contains("malformed document"), "{literal}");
    }

    /// A non-string payload must still be contained rather than escaping.
    #[test]
    fn a_non_string_panic_payload_is_still_contained() {
        let error = guard_engine_panic::<()>("odd", Path::new("f.rs"), || std::panic::panic_any(42_u8))
            .expect_err("a panic is converted to an error");
        assert!(error.to_string().contains("panicked"), "{error}");
    }
}
