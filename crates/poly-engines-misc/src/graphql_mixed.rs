//! Documents that mix executable definitions (`query`, `mutation`, `fragment`, …) with type-system
//! ones (`type`, `enum`, …).
//!
//! The spec allows both in one document, but `graphql-parser` parses only one family at a time.
//! This splits a document into its top-level definitions, blanks out one family at a time (keeping
//! every newline and column, so error locations stay exact) and parses each remainder.

use graphql_parser::{parse_query, parse_schema};

/// Why a mixed document failed to parse: the parser's own message.
pub(super) type ParseFailure = String;

/// `Some(Ok(()))` when the document mixes both families and each parses; `Some(Err(message))` when one
/// family fails; `None` when the document holds only one family (the caller's ordinary path applies).
pub(super) fn check(content: &str) -> Option<Result<(), ParseFailure>> {
    let definitions = split_definitions(content);
    let (executable, type_system): (Vec<_>, Vec<_>) = definitions
        .into_iter()
        .partition(|range| is_executable(&content[range.clone()]));
    if executable.is_empty() || type_system.is_empty() {
        return None;
    }
    let queries = mask(content, &executable);
    let types = mask(content, &type_system);
    if let Err(error) = parse_query::<&str>(&queries) {
        return Some(Err(error.to_string()));
    }
    if let Err(error) = parse_schema::<&str>(&types) {
        return Some(Err(error.to_string()));
    }
    Some(Ok(()))
}

fn is_executable(definition: &str) -> bool {
    let mut rest = definition;
    loop {
        rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix('#') {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    rest.starts_with('{')
        || ["query", "mutation", "subscription", "fragment"].iter().any(|keyword| {
            rest.strip_prefix(keyword)
                .is_some_and(|tail| !tail.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
}

/// Replace everything outside `keep` with spaces, preserving newlines.
fn mask(content: &str, keep: &[std::ops::Range<usize>]) -> String {
    let mut out = String::with_capacity(content.len());
    for (offset, ch) in content.char_indices() {
        if ch == '\n' || keep.iter().any(|range| range.contains(&offset)) {
            out.push(ch);
        } else {
            out.extend(std::iter::repeat_n(' ', ch.len_utf8()));
        }
    }
    out
}

/// Byte ranges of the top-level definitions: each ends at the `}` that returns brace depth to zero
/// (strings and comments skipped). Brace-less definitions (`scalar`, `union`, `directive`) travel
/// with the next braced one, which is why a trailing remainder is its own range.
fn split_definitions(content: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = content.as_bytes();
    let mut ranges = Vec::new();
    let (mut start, mut depth, mut i) = (0usize, 0usize, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'"' if content[i..].starts_with("\"\"\"") => {
                i += 3;
                while i < bytes.len() && !content[i..].starts_with("\"\"\"") {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 3;
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    ranges.push(start..i + 1);
                    start = i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if start < content.len() && !content[start..].trim().is_empty() {
        ranges.push(start..content.len());
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_and_a_type_in_one_document_both_parse() {
        let doc = "query Q { a { b } }\n\n# types\ntype A { b: Int }\nenum E { X Y }\nfragment F on A { b }\n";
        assert_eq!(check(doc), Some(Ok(())));
    }

    #[test]
    fn a_syntax_error_in_either_family_is_reported_at_its_real_location() {
        let doc = "type A { b: Int }\nquery Q { a { } }\n";
        let Some(Err(message)) = check(doc) else {
            panic!("expected a failure");
        };
        assert!(message.contains("2:"), "{message}");
    }

    #[test]
    fn a_single_family_document_is_left_to_the_ordinary_path() {
        assert_eq!(check("type A { b: Int }\n"), None);
        assert_eq!(check("query Q { a }\n"), None);
    }
}
