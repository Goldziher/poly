//! `magic-number` (opt-in): a bare numeric literal that is not on the
//! allowlist (default `-1, 0, 1, 2, 10, 100`).
//!
//! Best-effort and grammar-generic: rather than a per-language table of
//! numeric-literal node kinds, this walks the already-parsed tree (reusing
//! whatever tree the caller parsed for nesting/complexity/definitions — no
//! extra parse) and matches any **leaf** node whose kind name contains
//! `int` or `float` (covering `integer_literal`, `int_literal`,
//! `decimal_integer_literal`, `float_literal`, `number`/`number_literal`,
//! …) case-insensitively, then parses its own text as an integer. Only
//! integer literals are checked against the allowlist — a float value is
//! reported whenever its text does not parse cleanly to one of the allowed
//! integers, which in practice means every float is reported unless it is
//! written as e.g. `0` (an edge case, not a correctness bug: floats are
//! rarely "magic" indices/limits the way bare integers are).
//!
//! Being opt-in and off by default (ADR 0027's shipping gate has not been run
//! against a corpus for this rule), a language this heuristic gets wrong
//! costs an opt-in false positive, not a default-on regression.

use tree_sitter::Node;

use crate::engine::{Diagnostic, Severity, Span};

/// Node-kind substrings (checked case-insensitively) that identify a numeric
/// literal leaf across the grammars this engine's other rules already parse.
const NUMERIC_KIND_MARKERS: &[&str] = &["int", "float", "number"];

/// Node kinds that contain one of [`NUMERIC_KIND_MARKERS`] but are not a bare
/// numeric literal (a type name, not a value) and must be excluded.
const EXCLUDED_KINDS: &[&str] = &["predefined_type", "type_identifier", "integral_type"];

/// Walk `root` and report every magic number not in `allow`.
pub fn scan(root: Node, source: &[u8], allow: &[i64]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    walk(root, source, allow, &mut out);
    out
}

fn walk(node: Node, source: &[u8], allow: &[i64], out: &mut Vec<Diagnostic>) {
    if node.child_count() == 0 && is_numeric_literal_kind(node.kind()) {
        if let Some(finding) = check(node, source, allow) {
            out.push(finding);
        }
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk(cursor.node(), source, allow, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn is_numeric_literal_kind(kind: &str) -> bool {
    if EXCLUDED_KINDS.contains(&kind) {
        return false;
    }
    let lower = kind.to_ascii_lowercase();
    NUMERIC_KIND_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn check(node: Node, source: &[u8], allow: &[i64]) -> Option<Diagnostic> {
    let text = node.utf8_text(source).ok()?;
    let normalized = text.trim().trim_end_matches(['u', 'U', 'l', 'L', 'i', 'f']);
    let value: i64 = normalized.replace('_', "").parse().ok()?;
    if allow.contains(&value) {
        return None;
    }
    let start = node.start_position();
    let end = node.end_position();
    Some(Diagnostic {
        engine: "quality".to_owned(),
        code: Some("magic-number".to_owned()),
        severity: Severity::Warning,
        title: format!("magic number `{text}`"),
        description: Some("extract this into a named constant, or add it to the allowlist".to_owned()),
        span: Some(Span {
            start_line: start.row as u32 + 1,
            start_col: start.column as u32 + 1,
            end_line: end.row as u32 + 1,
            end_col: end.column as u32 + 1,
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use tree_sitter::Parser;
    use tree_sitter_language_pack::get_language;

    use super::*;

    fn parse(grammar: &str, source: &str) -> tree_sitter::Tree {
        let language = get_language(grammar).expect("grammar available");
        let mut parser = Parser::new();
        parser.set_language(&language).expect("set_language");
        parser.parse(source, None).expect("parse")
    }

    const DEFAULT_ALLOW: &[i64] = &[-1, 0, 1, 2, 10, 100];

    #[test]
    fn flags_an_unallowed_integer_literal() {
        let source = "fn f() -> i32 {\n    42\n}\n";
        let tree = parse("rust", source);
        let findings = scan(tree.root_node(), source.as_bytes(), DEFAULT_ALLOW);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code.as_deref(), Some("magic-number"));
    }

    #[test]
    fn does_not_flag_allowlisted_numbers() {
        let source = "fn f() -> i32 {\n    let a = 0;\n    let b = 1;\n    let c = 100;\n    a + b + c\n}\n";
        let tree = parse("rust", source);
        assert!(scan(tree.root_node(), source.as_bytes(), DEFAULT_ALLOW).is_empty());
    }

    #[test]
    fn respects_a_custom_allowlist() {
        let source = "fn f() -> i32 {\n    42\n}\n";
        let tree = parse("rust", source);
        assert!(scan(tree.root_node(), source.as_bytes(), &[42]).is_empty());
    }
}
