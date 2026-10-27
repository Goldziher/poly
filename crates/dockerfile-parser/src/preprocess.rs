//! Input normalisation performed before the pest grammar sees a Dockerfile.
//!
//! The grammar only understands `\` line continuations and has no notion of heredocs or a byte order mark. Rather than
//! complicate it, the input is rewritten into a *masked* copy of identical byte length in which those constructs are
//! replaced by blanks. Every offset in the parse tree therefore indexes the original input as well.

use std::sync::LazyLock;

use regex::Regex;

use crate::splicer::Span;
use crate::util::SpannedString;

/// A heredoc attached to a `RUN`, `COPY` or `ADD` instruction (`RUN <<EOF ... EOF`).
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Heredoc {
    /// The index into `Dockerfile::instructions` of the instruction that opened this heredoc.
    pub instruction: usize,
    /// The word that terminates the heredoc.
    pub delimiter: String,
    /// Whether the heredoc was opened with `<<-`, which strips leading tabs from the body and the delimiter line.
    pub strip_tabs: bool,
    /// Whether the delimiter was quoted, which disables variable expansion in the body.
    pub quoted: bool,
    /// The body lines, excluding the delimiter line.
    pub body: SpannedString,
    /// The body together with the closing delimiter line.
    pub span: Span,
}

/// A heredoc found while scanning, before it is attached to an instruction.
#[derive(Debug)]
pub(crate) struct RawHeredoc {
    pub(crate) delimiter: String,
    pub(crate) strip_tabs: bool,
    pub(crate) quoted: bool,
    pub(crate) body: Span,
    pub(crate) span: Span,
}

/// The result of [`prepare`].
pub(crate) struct Prepared {
    /// A copy of the input, byte for byte the same length, that the grammar can parse.
    pub(crate) masked: String,
    /// The heredocs that were blanked out of `masked`, in input order.
    pub(crate) heredocs: Vec<RawHeredoc>,
}

static HEREDOC_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<<(-?)(?:"([A-Za-z_][A-Za-z0-9_.-]*)"|'([A-Za-z_][A-Za-z0-9_.-]*)'|([A-Za-z_][A-Za-z0-9_.-]*))"#)
        .expect("static regex is valid")
});

const BOM: &str = "\u{feff}";

/// A physical line: its byte range excluding the line terminator.
struct Line {
    start: usize,
    end: usize,
}

fn lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text[start..].find('\n').map_or(text.len(), |offset| start + offset);
        lines.push(Line { start, end });
        start = end + 1;
    }
    lines
}

fn blank(buffer: &mut [u8], range: std::ops::Range<usize>) {
    for byte in &mut buffer[range] {
        if !matches!(*byte, b'\n' | b'\r') {
            *byte = b' ';
        }
    }
}

fn content_of(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

fn is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim_start_matches([' ', '\t']);
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// Returns the escape character declared by a leading `# escape=` parser directive, if it is a backtick.
fn escape_is_backtick(text: &str) -> bool {
    for line in text.lines() {
        let Some(directive) = line.trim().strip_prefix('#') else {
            return false;
        };
        let Some((name, value)) = directive.split_once('=') else {
            return false;
        };
        if name.trim().eq_ignore_ascii_case("escape") {
            return value.trim() == "`";
        }
    }
    false
}

/// Rewrites `text` so the grammar can parse it, and extracts the heredocs it contains.
pub(crate) fn prepare(text: &str) -> Prepared {
    let mut buffer = text.as_bytes().to_vec();

    if text.starts_with(BOM) {
        blank(&mut buffer, 0..BOM.len());
    }

    if escape_is_backtick(text.strip_prefix(BOM).unwrap_or(text)) {
        remap_backtick_escapes(text, &mut buffer);
    }

    // Only ASCII bytes were overwritten with ASCII bytes, so the buffer is still valid UTF-8.
    let working = String::from_utf8(buffer).unwrap_or_else(|_| text.to_string());
    let heredocs = find_heredocs(&working);

    let mut buffer = working.into_bytes();
    for heredoc in &heredocs {
        blank(&mut buffer, heredoc.span.start..heredoc.span.end);
    }

    Prepared {
        masked: String::from_utf8(buffer).unwrap_or_else(|_| text.to_string()),
        heredocs,
    }
}

/// With `# escape=\``, a trailing backtick continues the line and a trailing backslash is an ordinary character. Swap
/// them so the grammar's `\` handling applies unchanged.
fn remap_backtick_escapes(text: &str, buffer: &mut [u8]) {
    for line in lines(text) {
        let content = content_of(&text[line.start..line.end]);
        if is_blank_or_comment(content) {
            continue;
        }
        let trimmed = content.trim_end_matches([' ', '\t']);
        let Some(last) = trimmed.len().checked_sub(1) else {
            continue;
        };
        match trimmed.as_bytes()[last] {
            b'`' => buffer[line.start + last] = b'\\',
            b'\\' => buffer[line.start + last] = b' ',
            _ => {}
        }
    }
}

/// The text of a line without its trailing whitespace and without a trailing continuation backslash, and whether the
/// line continued.
fn strip_continuation(line: &str) -> (&str, bool) {
    let trimmed = content_of(line).trim_end_matches([' ', '\t']);
    match trimmed.strip_suffix('\\') {
        Some(rest) => (rest, true),
        None => (trimmed, false),
    }
}

fn find_heredocs(text: &str) -> Vec<RawHeredoc> {
    let physical = lines(text);
    let text_of = |index: usize| &text[physical[index].start..physical[index].end];
    let mut found = Vec::new();
    let mut index = 0;

    while index < physical.len() {
        if is_blank_or_comment(content_of(text_of(index))) {
            index += 1;
            continue;
        }

        let mut logical = String::new();
        let mut cursor = index;
        loop {
            let (piece, continued) = strip_continuation(text_of(cursor));
            logical.push_str(piece);
            logical.push(' ');
            cursor += 1;
            if !continued {
                break;
            }
            while cursor < physical.len() && is_blank_or_comment(content_of(text_of(cursor))) {
                cursor += 1;
            }
            if cursor >= physical.len() {
                break;
            }
        }

        let keyword = logical.split_whitespace().next().unwrap_or("");
        if ["run", "copy", "add"]
            .iter()
            .any(|name| keyword.eq_ignore_ascii_case(name))
        {
            for marker in HEREDOC_MARKER.captures_iter(&logical) {
                let start = marker.get(0).map_or(0, |whole| whole.start());
                if logical[..start].ends_with('<') {
                    continue;
                }
                let strip_tabs = !marker[1].is_empty();
                let (delimiter, quoted) = match (marker.get(2).or_else(|| marker.get(3)), marker.get(4)) {
                    (Some(word), _) => (word.as_str(), true),
                    (_, Some(word)) => (word.as_str(), false),
                    _ => continue,
                };

                if let Some(heredoc) = read_body(text, &physical, &mut cursor, delimiter, strip_tabs, quoted) {
                    found.push(heredoc);
                }
            }
        }

        index = cursor.max(index + 1);
    }

    found
}

/// Consumes the body lines starting at `*cursor` up to and including the delimiter line.
fn read_body(
    text: &str,
    physical: &[Line],
    cursor: &mut usize,
    delimiter: &str,
    strip_tabs: bool,
    quoted: bool,
) -> Option<RawHeredoc> {
    let first = physical.get(*cursor)?;
    let body_start = first.start;
    let mut index = *cursor;

    while let Some(line) = physical.get(index) {
        let content = content_of(&text[line.start..line.end]);
        let candidate = if strip_tabs {
            content.trim_start_matches('\t')
        } else {
            content
        };
        if candidate == delimiter {
            *cursor = index + 1;
            return Some(RawHeredoc {
                delimiter: delimiter.to_string(),
                strip_tabs,
                quoted,
                body: Span::new(body_start, line.start),
                span: Span::new(body_start, line.end),
            });
        }
        index += 1;
    }

    // Unterminated: the rest of the file is the body.
    let end = text.len();
    *cursor = physical.len();
    Some(RawHeredoc {
        delimiter: delimiter.to_string(),
        strip_tabs,
        quoted,
        body: Span::new(body_start, end),
        span: Span::new(body_start, end),
    })
}
