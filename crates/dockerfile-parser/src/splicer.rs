// (C) Copyright 2019-2020 Hewlett Packard Enterprise Development LP

use std::convert::TryInto;
use std::fmt;

use crate::dockerfile_parser::Dockerfile;
use crate::error::{Error, Result};
use crate::parser::Pair;

/// An offset used to adjust proceeding Spans after content has been spliced
#[derive(Debug)]
struct SpliceOffset {
    position: usize,
    offset: isize,
}

/// A byte-index tuple representing a span of characters in a string
#[derive(PartialEq, Eq, Clone, Ord, PartialOrd, Copy)]
pub struct Span {
    /// The byte offset of the first byte of the span.
    pub start: usize,
    /// The byte offset one past the last byte of the span.
    pub end: usize,
}

impl Span {
    /// Creates a new value.
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    pub(crate) fn from_pair(record: &Pair) -> Span {
        let pest_span = record.as_span();

        Span {
            start: pest_span.start(),
            end: pest_span.end(),
        }
    }

    fn adjust_offsets(&self, offsets: &[SpliceOffset]) -> Span {
        let mut start = self.start as isize;
        let mut end = self.end as isize;

        for splice in offsets {
            if splice.position < start as usize {
                start += splice.offset;
                end += splice.offset;
            } else if splice.position < end as usize {
                end += splice.offset;
            }
        }

        Span {
            start: start.try_into().ok().unwrap_or(0),
            end: end.try_into().ok().unwrap_or(0),
        }
    }

    /// Determines the 0-indexed line number and line-relative position of this
    /// span.
    ///
    /// A reference to the Dockerfile is necessary to examine the original input
    /// string. Note that if the original span crosses a newline boundary, the
    /// relative span's `end` field will be larger than the line length.
    pub fn relative_span(&self, dockerfile: &Dockerfile) -> (usize, Span) {
        let mut line_start_offset = 0;
        let mut lines = 0;
        for (i, c) in dockerfile.content.as_bytes().iter().enumerate() {
            if i == self.start {
                break;
            }

            if *c == b'\n' {
                lines += 1;
                line_start_offset = i + 1;
            }
        }

        let start = self.start - line_start_offset;
        let end = start + (self.end - self.start);

        (lines, Span { start, end })
    }
}

impl From<(usize, usize)> for Span {
    fn from(tup: (usize, usize)) -> Span {
        Span::new(tup.0, tup.1)
    }
}

impl From<&Pair<'_>> for Span {
    fn from(pair: &Pair<'_>) -> Self {
        Span::from_pair(pair)
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("").field(&self.start).field(&self.end).finish()
    }
}

/// A utility to repeatedly replace spans of text within a larger document.
///
/// Each subsequent call to `Splicer::splice(...)` rewrites the `content` buffer
/// and appends to the list of internal offsets. `Splicer::splice(...)` then
/// adjusts span bounds at call-time to ensures repeated calls to `splice(...)`
/// continue to work even if one or both of the span bounds have shifted.
///
/// # Example
/// ```
/// use dockerfile_parser::*;
///
/// let dockerfile: Dockerfile = r#"
///   FROM alpine:3.10
/// "#.parse()?;
///
/// let from = match &dockerfile.instructions[0] {
///   Instruction::From(f) => f,
///   _ => panic!("invalid")
/// };
///
/// let mut splicer = dockerfile.splicer();
/// splicer.splice(&from.image.span, "alpine:3.11")?;
///
/// assert_eq!(splicer.content, r#"
///   FROM alpine:3.11
/// "#);
/// # Ok::<(), dockerfile_parser::Error>(())
/// ```
pub struct Splicer {
    /// The current content of the splice buffer.
    pub content: String,

    splice_offsets: Vec<SpliceOffset>,
}

impl Splicer {
    /// Creates a new Splicer from the given Dockerfile.
    pub(crate) fn from(dockerfile: &Dockerfile) -> Splicer {
        Splicer {
            content: dockerfile.content.clone(),
            splice_offsets: Vec::new(),
        }
    }

    pub(crate) fn from_str(s: &str) -> Splicer {
        Splicer {
            content: s.to_string(),
            splice_offsets: Vec::new(),
        }
    }

    /// Replaces a Span with the given replacement string, mutating the `content`
    /// string.
    ///
    /// Sections may be deleted by replacing them with an empty string (`""`).
    ///
    /// Note that spans are always relative to the *original input document*.
    /// Span offsets are recalculated at call-time to account for previous calls
    /// to `splice(...)` that may have shifted one or both of the span bounds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSpan`] when the span is reversed, extends past the
    /// buffer or does not fall on character boundaries; the buffer is left
    /// unchanged.
    pub fn splice(&mut self, span: &Span, replacement: &str) -> Result<()> {
        let invalid = || Error::InvalidSpan {
            start: span.start,
            end: span.end,
            len: self.content.len(),
        };
        if span.start > span.end {
            return Err(invalid());
        }
        let adjusted = span.adjust_offsets(&self.splice_offsets);
        if adjusted.start > adjusted.end
            || !self.content.is_char_boundary(adjusted.start)
            || !self.content.is_char_boundary(adjusted.end)
        {
            return Err(invalid());
        }

        // determine the splice offset (only used on subsequent splices)
        let prev_len = adjusted.end - adjusted.start;
        let new_len = replacement.len();
        let offset = new_len as isize - prev_len as isize;
        self.splice_offsets.push(SpliceOffset {
            position: adjusted.start,
            offset,
        });

        self.content.replace_range(adjusted.start..adjusted.end, replacement);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::*;
    use indoc::indoc;
    use std::convert::TryInto;

    #[test]
    fn splice_rejects_invalid_spans_without_panicking() {
        let dockerfile = Dockerfile::parse("FROM alpine\n").unwrap();
        let mut splicer = dockerfile.splicer();

        for span in [Span::new(5, 2), Span::new(0, 999), Span::new(998, 999)] {
            assert!(
                matches!(splicer.splice(&span, "x"), Err(Error::InvalidSpan { .. })),
                "{span:?}"
            );
        }
        assert_eq!(
            splicer.content, "FROM alpine\n",
            "a rejected splice leaves the buffer untouched"
        );
    }

    #[test]
    fn splice_rejects_a_span_inside_a_multibyte_character() {
        let dockerfile = Dockerfile::parse("FROM alpine\n# \u{e9}\n").unwrap();
        let mut splicer = dockerfile.splicer();
        let inside = dockerfile.content.find('\u{e9}').unwrap() + 1;

        assert!(splicer.splice(&Span::new(inside, inside + 1), "x").is_err());
    }

    #[test]
    fn test_relative_span() {
        let d = Dockerfile::parse(indoc!(
            r#"
      FROM alpine:3.10 as build
      FROM alpine:3.10

      RUN echo "hello world"

      COPY --from=build /foo /bar
    "#
        ))
        .unwrap();

        let first_from = TryInto::<&FromInstruction>::try_into(&d.instructions[0]).unwrap();
        assert_eq!(
            first_from.alias.as_ref().unwrap().span.relative_span(&d),
            (0, (20, 25).into())
        );

        let copy = TryInto::<&CopyInstruction>::try_into(&d.instructions[3]).unwrap();

        let len = copy.span.end - copy.span.start;
        let content = &d.content[copy.span.start..copy.span.end];

        let (rel_line_index, rel_span) = copy.span.relative_span(&d);
        let rel_len = rel_span.end - rel_span.start;
        assert_eq!(len, rel_len);

        let rel_line = d.content.lines().collect::<Vec<&str>>()[rel_line_index];
        let rel_content = &rel_line[rel_span.start..rel_span.end];
        assert_eq!(rel_line, "COPY --from=build /foo /bar");
        assert_eq!(content, rel_content);

        // COPY --from=build /foo /bar
        assert_eq!(copy.span.relative_span(&d), (5, (0, 27).into()));

        // --from=build
        assert_eq!(copy.flags[0].span.relative_span(&d), (5, (5, 17).into()));

        // build
        assert_eq!(copy.flags[0].value.span.relative_span(&d), (5, (12, 17).into()));
    }
}
