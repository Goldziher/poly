//! oxc JSON/JSONC validation: strict parse with comments and trailing commas tolerated for JSONC.

use crate::engine::{Diagnostic, Severity, SourceFile, Span};
use crate::language::Language;

pub(super) fn lint_json(src: &SourceFile) -> anyhow::Result<Vec<Diagnostic>> {
    // JSONC permits comments *and* trailing commas — both valid in the spec our
    // formatter targets, and the JSONC formatter itself emits/preserves trailing
    // commas. `serde_json` is strict JSON, so it rejects both. Neutralise them
    // (replace with spaces, preserving byte offsets so any *genuine* parse error
    // still reports at the right position) before the strict parse. Plain `.json`
    // keeps strict semantics: a trailing comma there is a real error.
    let text = if src.language == Language::Jsonc {
        neutralize_trailing_commas(&strip_jsonc_comments(&src.content))
    } else {
        src.content.to_string()
    };

    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(_) => Ok(vec![]),
        Err(err) => {
            let line = err.line() as u32;
            let col = err.column() as u32;
            Ok(vec![Diagnostic {
                engine: "oxc".to_owned(),
                code: Some("parse-error".to_owned()),
                title: err.to_string(),
                description: None,
                url: None,
                severity: Severity::Error,
                span: Some(Span {
                    start_line: line,
                    start_col: col,
                    end_line: line,
                    end_col: col,
                }),
                fix: vec![],
                metadata: Default::default(),
            }])
        }
    }
}

/// Strip `//` and `/* */` comments from JSONC, preserving string contents and
/// character positions (comments are replaced with spaces so offsets stay valid).
fn strip_jsonc_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                out.push('"');
                loop {
                    match chars.next() {
                        None => break,
                        Some('\\') => {
                            out.push('\\');
                            if let Some(escaped) = chars.next() {
                                out.push(escaped);
                            }
                        }
                        Some('"') => {
                            out.push('"');
                            break;
                        }
                        Some(c) => out.push(c),
                    }
                }
            }
            '/' => match chars.peek() {
                Some('/') => {
                    chars.next();
                    out.push(' ');
                    out.push(' ');
                    for c in chars.by_ref() {
                        if c == '\n' {
                            out.push('\n');
                            break;
                        } else {
                            out.push(' ');
                        }
                    }
                }
                Some('*') => {
                    chars.next();
                    out.push(' ');
                    out.push(' ');
                    let mut prev = ' ';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            out.push(' ');
                            break;
                        }
                        out.push(if c == '\n' { '\n' } else { ' ' });
                        prev = c;
                    }
                }
                _ => out.push('/'),
            },
            other => out.push(other),
        }
    }

    out
}

/// Replace JSONC **trailing commas** — a `,` whose next non-whitespace character
/// is `}` or `]` — with a space, so strict `serde_json` accepts them while byte
/// offsets (and therefore any genuine parse-error position) are preserved.
///
/// Operates on comment-stripped input (comments are already spaces) and is
/// string-aware: commas and brackets inside string literals are ignored.
fn neutralize_trailing_commas(src: &str) -> String {
    let mut bytes: Vec<u8> = src.as_bytes().to_vec();
    // Byte index of the most recent structural `,` with only whitespace since.
    let mut pending_comma: Option<usize> = None;
    let mut in_string = false;
    let mut escaped = false;

    for i in 0..bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                pending_comma = None;
            }
            b',' => pending_comma = Some(i),
            b'}' | b']' => {
                if let Some(j) = pending_comma.take() {
                    bytes[j] = b' ';
                }
            }
            _ if b.is_ascii_whitespace() => {}
            _ => pending_comma = None,
        }
    }

    // SAFETY-equivalent: we only ever overwrite an ASCII `,` with an ASCII space,
    // so the buffer remains valid UTF-8.
    String::from_utf8(bytes).expect("blanking ASCII commas keeps valid UTF-8")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_src(content: &str, lang: Language) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test.js"),
            language: lang,
            content: content.into(),
        }
    }

    #[test]
    fn valid_json_produces_no_diagnostics() {
        let src = make_src(r#"{"a":1}"#, Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn invalid_json_produces_parse_error() {
        let src = make_src(r#"{"a":1,}"#, Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty());
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn jsonc_with_comments_is_valid() {
        let src = make_src("{\n  // comment\n  \"a\": 1\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty(), "got diags: {diags:?}");
    }

    #[test]
    fn jsonc_with_trailing_commas_is_valid() {
        // Object, array, and nested trailing commas — all valid JSONC, all of
        // which the JSONC formatter itself emits/preserves.
        let src = make_src("{\n  \"a\": 1,\n  \"b\": [1, 2,],\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(diags.is_empty(), "trailing commas are valid JSONC; got: {diags:?}");
    }

    #[test]
    fn jsonc_genuinely_invalid_still_errors() {
        // A real syntax error (missing value) must still be reported.
        let src = make_src("{\n  \"a\":\n}\n", Language::Jsonc);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty(), "malformed JSONC must still error");
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn plain_json_trailing_comma_still_errors() {
        // Strict `.json` keeps strict semantics — trailing commas are invalid.
        let src = make_src("{\"a\": 1,}", Language::Json);
        let diags = lint_json(&src).unwrap();
        assert!(!diags.is_empty(), "trailing comma is invalid in strict JSON");
        assert_eq!(diags[0].code, Some("parse-error".to_owned()));
    }

    #[test]
    fn neutralize_ignores_comma_inside_string() {
        // A `,` followed by `]` *inside a string* is not a trailing comma.
        let input = r#"{"a": "x,]", "b": [1,]}"#;
        let out = neutralize_trailing_commas(input);
        // The in-string `,` survives; the real trailing `,` before `]` is blanked.
        assert_eq!(out, r#"{"a": "x,]", "b": [1 ]}"#);
    }

    #[test]
    fn strip_jsonc_preserves_string_slashes() {
        let input = r#"{"url": "http://example.com"}"#;
        let stripped = strip_jsonc_comments(input);
        assert_eq!(stripped, input);
    }
}
