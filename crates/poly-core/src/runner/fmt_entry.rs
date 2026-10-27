//! The on-disk shape of a cached formatter step.
//!
//! A step whose output equals its input — nearly every step on a repository that
//! is already formatted — is stored as a single tag byte instead of a second copy
//! of the file. Whatever follows `TEXT` is the formatted output, verbatim.

use std::sync::Arc;

const UNCHANGED: u8 = 0;
const TEXT: u8 = 1;

/// Encode the step that turned `input` into `output`.
pub(super) fn encode(input: &str, output: &str) -> Vec<u8> {
    if input == output {
        return vec![UNCHANGED];
    }
    let mut bytes = Vec::with_capacity(output.len() + 1);
    bytes.push(TEXT);
    bytes.extend_from_slice(output.as_bytes());
    bytes
}

/// The output of a stored step applied to `input`, or `None` if the entry is
/// not one this build wrote.
pub(super) fn decode(input: &Arc<str>, bytes: &[u8]) -> Option<Arc<str>> {
    match bytes.split_first()? {
        (&UNCHANGED, []) => Some(Arc::clone(input)),
        (&TEXT, text) => std::str::from_utf8(text).ok().map(Arc::from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchanged_step_costs_one_byte() {
        assert_eq!(encode("fn main() {}\n", "fn main() {}\n"), vec![UNCHANGED]);
    }

    #[test]
    fn a_changed_step_round_trips() {
        let input: Arc<str> = Arc::from("a  =  1\n");
        let stored = encode(&input, "a = 1\n");
        assert_eq!(decode(&input, &stored).as_deref(), Some("a = 1\n"));
    }

    #[test]
    fn an_unchanged_step_replays_the_input_it_is_applied_to() {
        let input: Arc<str> = Arc::from("x\n");
        assert_eq!(decode(&input, &encode("x\n", "x\n")).as_deref(), Some("x\n"));
    }

    #[test]
    fn formatting_a_file_to_nothing_is_not_confused_with_unchanged() {
        let input: Arc<str> = Arc::from("\n\n");
        let stored = encode(&input, "");
        assert_eq!(decode(&input, &stored).as_deref(), Some(""));
    }

    #[test]
    fn malformed_entries_are_rejected() {
        let input: Arc<str> = Arc::from("x");
        assert!(decode(&input, &[]).is_none());
        assert!(decode(&input, &[UNCHANGED, b'x']).is_none());
        assert!(decode(&input, &[9, b'x']).is_none());
        assert!(decode(&input, &[TEXT, 0xff]).is_none());
    }
}
