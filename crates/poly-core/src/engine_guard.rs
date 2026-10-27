//! Containment for panics raised inside a wrapped upstream parser.
//!
//! poly compiles roughly twenty third-party parsers in-process. Their public
//! APIs return `Result`, but their internals `assert!`: `biome_parser` used to
//! assert on a GraphQL *fragment description* — a real language feature
//! (graphql-js #4482), fixed upstream since — and sqruff's
//! `FluffConfig::from_source` panics rather than errors on a malformed
//! document. poly does not control that code and cannot audit every edge case
//! in it, which is why the tests drive this guard with an engine that panics on
//! demand rather than with whichever upstream input happens to panic today.
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
//! # Stack exhaustion
//!
//! A panic can be caught; a stack overflow cannot — the runtime aborts the
//! process before any handler runs, with the same total loss. Several in-process
//! parsers recurse once per nesting level (`hcl-rs`, `malva`) and overflow the
//! worker stack on input a few thousand brackets deep, which fuzzing produced
//! from truncated files. [`refuse_pathological_nesting`] turns that input into
//! the same per-file error before those engines see it. It is called by the
//! engines that need it rather than by the runner for every file, because a
//! bracket count is only meaningful where brackets are syntax: a shell script
//! printing ANSI escapes (`\e[97m`) has hundreds of unclosed `[` and nothing
//! nested at all.
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

/// Bracket nesting beyond which a file is refused rather than parsed.
///
/// Balanced input 600 deep parses cleanly in every backend tried and 1200 deep
/// overflows the 16 MiB worker stack in two of them, so this sits well under
/// the failure and far above anything written by hand or emitted by a tool.
const MAX_NESTING_DEPTH: usize = 400;

/// Refuse `content` when its bracket nesting would exhaust an engine's stack.
///
/// Counts `(`, `[` and `{` against their closers without regard to quoting, so
/// it is for engines whose language makes brackets syntax. A balanced run
/// inside a string never accumulates; only unclosed or genuinely nested input
/// reaches the limit.
pub(crate) fn refuse_pathological_nesting(path: &Path, content: &str) -> anyhow::Result<()> {
    let mut depth = 0usize;
    for byte in content.bytes() {
        match byte {
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > MAX_NESTING_DEPTH {
                    anyhow::bail!(
                        "{} nests brackets more than {MAX_NESTING_DEPTH} levels deep; it was not checked because \
                         parsing it could overflow the stack and abort the whole run",
                        path.display()
                    );
                }
            }
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Whether `content` nests more than `depth` brackets deep and leaves some
/// unclosed.
///
/// For parsers that backtrack exponentially on unclosed nesting — a few dozen
/// levels take minutes and a few more never return, wedging a worker and with
/// it the run — where the same depth *closed* parses in linear time. Like
/// [`refuse_pathological_nesting`] it ignores quoting; a false positive costs
/// one file's formatting, a false negative costs the run.
pub(crate) fn deep_unbalanced_nesting(content: &str, depth: usize) -> bool {
    let (mut open, mut deepest, mut underflowed) = (0usize, 0usize, false);
    for byte in content.bytes() {
        match byte {
            b'(' | b'[' | b'{' => {
                open += 1;
                deepest = deepest.max(open);
            }
            b')' | b']' | b'}' => match open.checked_sub(1) {
                Some(remaining) => open = remaining,
                None => underflowed = true,
            },
            _ => {}
        }
    }
    deepest > depth && (open != 0 || underflowed)
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

    #[test]
    fn unclosed_brackets_past_the_limit_are_refused_with_the_file_named() {
        let content = format!("a = {}\n", "[".repeat(MAX_NESTING_DEPTH + 1));
        let error = refuse_pathological_nesting(Path::new("deep.tf"), &content).expect_err("too deep");
        assert!(error.to_string().contains("deep.tf"), "{error}");
        assert!(error.to_string().contains("not checked"), "{error}");
    }

    #[test]
    fn deep_nesting_is_only_suspect_when_something_is_left_open() {
        let closed = format!("{}1{}", "(".repeat(30), ")".repeat(30));
        assert!(
            !deep_unbalanced_nesting(&closed, 12),
            "closed nesting parses in linear time"
        );
        assert!(deep_unbalanced_nesting(&format!("{}1", "(".repeat(30)), 12));
        assert!(
            !deep_unbalanced_nesting(&format!("{}1", "(".repeat(5)), 12),
            "shallow input is never suspect"
        );
        assert!(deep_unbalanced_nesting(
            &format!("){}1{}", "(".repeat(30), ")".repeat(30)),
            12
        ));
    }

    #[test]
    fn balanced_nesting_up_to_the_limit_and_long_flat_runs_pass() {
        let nested = format!("{}{}", "(".repeat(MAX_NESTING_DEPTH), ")".repeat(MAX_NESTING_DEPTH));
        refuse_pathological_nesting(Path::new("ok.scss"), &nested).expect("at the limit");

        let flat = "()".repeat(MAX_NESTING_DEPTH * 10);
        refuse_pathological_nesting(Path::new("flat.txt"), &flat).expect("balanced pairs never accumulate");
    }

    /// A non-string payload must still be contained rather than escaping.
    #[test]
    fn a_non_string_panic_payload_is_still_contained() {
        let error = guard_engine_panic::<()>("odd", Path::new("f.rs"), || std::panic::panic_any(42_u8))
            .expect_err("a panic is converted to an error");
        assert!(error.to_string().contains("panicked"), "{error}");
    }
}
