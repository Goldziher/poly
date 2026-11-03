//! Panic-payload and nesting-depth helpers shared by the engine guard in `poly-core` and any component host.

use std::path::Path;

/// Best-effort recovery of a panic payload's message.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else {
        "panic payload was not a string".to_owned()
    }
}

/// Bracket nesting beyond which a file is refused rather than parsed.
///
/// Balanced input 600 deep parses cleanly in every backend tried and 1200 deep
/// overflows the 16 MiB worker stack in two of them, so this sits well under
/// the failure and far above anything written by hand or emitted by a tool.
pub const MAX_NESTING_DEPTH: usize = 400;

/// Refuse `content` when its bracket nesting would exhaust an engine's stack.
///
/// Counts `(`, `[` and `{` against their closers without regard to quoting, so
/// it is for engines whose language makes brackets syntax. A balanced run
/// inside a string never accumulates; only unclosed or genuinely nested input
/// reaches the limit.
pub fn refuse_pathological_nesting(path: &Path, content: &str) -> anyhow::Result<()> {
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
pub fn deep_unbalanced_nesting(content: &str, depth: usize) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
