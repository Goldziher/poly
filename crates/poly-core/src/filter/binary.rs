//! Whether a file is binary, decided before poly tries to read it as text.
//!
//! poly discovers files by path, and a path is a weak claim about content. A
//! compiled gettext catalog (`.mo`), a `.pyc`, a PNG saved under a `.txt` name
//! — each reaches the runner looking like any other discovered file, and the
//! first thing the runner did with it was `String::from_utf8`. That call fails,
//! and the failure was reported as *malformed text*: an error-severity
//! `invalid-utf8` diagnostic on the lint path and a per-file error on the
//! format path. Django's 1,263 `.mo` catalogs turned `poly fmt --check` into
//! exit 2 on a repository with nothing wrong in it.
//!
//! The two cases need different answers because they call for different action.
//! Malformed *text* is a defect in a file someone wrote and poly should say so.
//! A binary artifact is not a defect at all — poly simply had no business
//! opening it — so it belongs in the skip accounting, where `--deny-skips` can
//! still see it, and not in the finding count.

/// How much of the file the scan reads.
///
/// A NUL byte is the near-universal separator between text and binary, and it
/// occurs within the first few bytes of essentially every real binary format —
/// magic numbers and length-prefixed headers are full of them. Scanning the
/// whole file to find one would cost the hot path a full pass over every
/// artifact in the tree for no additional discrimination, so the window is
/// bounded. The value matches what `git` uses for the same decision.
const SCAN_LIMIT: usize = 8000;

/// Does `bytes` look like a binary file rather than text?
///
/// True when a NUL byte appears in the first [`SCAN_LIMIT`] bytes.
///
/// Deliberately **not** "is not valid UTF-8". Latin-1 source, a truncated
/// multi-byte sequence, and a file that lost a byte in transit are all invalid
/// UTF-8 and all things a reader wants reported; treating them as binary would
/// silently drop the report. Requiring a NUL keeps the carve-out to files that
/// were never text, which is why
/// [`crate::runner`] can skip on this answer without weakening the
/// `invalid-utf8` diagnostic that
/// `tests/invalid_utf8.rs` pins.
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SCAN_LIMIT).any(|&byte| byte == 0)
}

#[cfg(test)]
mod tests {
    use super::is_binary;

    #[test]
    fn a_nul_byte_marks_the_content_binary() {
        assert!(is_binary(b"\xde\x12\x04\x95\x00\x00\x00\x00"));
    }

    #[test]
    fn plain_text_is_not_binary() {
        assert!(!is_binary(b"fn main() {}\n"));
    }

    #[test]
    fn malformed_utf8_without_a_nul_is_not_binary() {
        // The distinction the whole module exists for: this must keep reporting
        // as a text decode failure, not disappear into the skip list.
        assert!(!is_binary(b"x = 1\n\xff\xfe not utf-8\n"));
    }

    #[test]
    fn a_nul_beyond_the_scan_window_is_not_reached() {
        // Documents the bound rather than asserting it is the ideal one: a file
        // whose first 8000 bytes are clean text is treated as text.
        let mut content = vec![b'a'; super::SCAN_LIMIT];
        content.push(0);
        assert!(!is_binary(&content));
    }

    #[test]
    fn a_nul_at_the_last_scanned_byte_is_found() {
        let mut content = vec![b'a'; super::SCAN_LIMIT - 1];
        content.push(0);
        assert!(is_binary(&content));
    }
}
