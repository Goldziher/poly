//! Byte offset to 1-based `(line, column)` conversion that stays cheap when a
//! file produces thousands of diagnostics.
//!
//! The naive scan from the start of the file costs O(file) per diagnostic, which
//! turns a 1 MB minified bundle with thousands of findings into billions of
//! steps. [`LineIndex`] pays one O(file) pass up front and then answers each
//! query in O(log lines + [`CHUNK`]) regardless of how long the line is.

/// Granularity of the character-count checkpoints, in bytes.
const CHUNK: usize = 1024;

/// Precomputed line starts and character-count checkpoints for one source text.
pub(crate) struct LineIndex<'a> {
    src: &'a str,
    line_starts: Vec<usize>,
    /// `chars_before[k]` is the number of characters starting before byte `k * CHUNK`.
    chars_before: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        let bytes = src.as_bytes();
        let mut line_starts = vec![0];
        line_starts.extend(bytes.iter().enumerate().filter(|&(_, &b)| b == b'\n').map(|(i, _)| i + 1));
        let mut chars_before = Vec::with_capacity(bytes.len() / CHUNK + 1);
        let mut total = 0usize;
        chars_before.push(0);
        for chunk in bytes.chunks(CHUNK) {
            total += count_char_starts(chunk);
            chars_before.push(total);
        }
        Self {
            src,
            line_starts,
            chars_before,
        }
    }

    /// 1-based `(line, column)` of `offset`, with the column counted in
    /// characters. An offset past the end clamps to the end of the text.
    pub(crate) fn line_col(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.src.len());
        let line_index = self.line_starts.partition_point(|&start| start <= offset) - 1;
        let line_start = self.line_starts[line_index];
        let col = self.char_starts_before(offset) - self.char_starts_before(line_start);
        (line_index as u32 + 1, col as u32 + 1)
    }

    fn char_starts_before(&self, offset: usize) -> usize {
        let chunk_start = offset - offset % CHUNK;
        self.chars_before[offset / CHUNK] + count_char_starts(&self.src.as_bytes()[chunk_start..offset])
    }
}

fn count_char_starts(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| (b & 0xC0) != 0x80).count()
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    fn reference(src: &str, offset: usize) -> (u32, u32) {
        let safe_offset = offset.min(src.len());
        let (mut line, mut col) = (1u32, 1u32);
        for (i, ch) in src.char_indices() {
            if i >= safe_offset {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    #[test]
    fn matches_the_linear_scan_on_every_boundary() {
        let long_line = "é€x".repeat(900);
        let src = format!("ab\n{long_line}\n\nlast é line\n{long_line}");
        let index = LineIndex::new(&src);
        for offset in (0..=src.len() + 3).filter(|&o| o > src.len() || src.is_char_boundary(o)) {
            assert_eq!(index.line_col(offset), reference(&src, offset), "offset {offset}");
        }
    }

    #[test]
    fn empty_text_is_line_one_column_one() {
        assert_eq!(LineIndex::new("").line_col(0), (1, 1));
        assert_eq!(LineIndex::new("").line_col(9), (1, 1));
    }

    #[test]
    fn offset_just_after_a_newline_starts_the_next_line() {
        assert_eq!(LineIndex::new("a\nb").line_col(2), (2, 1));
    }
}
