//! `law-of-demeter` (opt-in): a chain of member/attribute accesses longer
//! than the configured depth (default 3), e.g. `a.b.c.d.e`.
//!
//! Best-effort and grammar-generic, like [`super::magic_number`]: rather than
//! a per-language table, this matches any node whose kind name contains
//! `member_expression`, `field_expression`, `attribute`, or
//! `selector_expression` — the common tree-sitter names for `a.b` access
//! across the languages this engine covers — and counts how many such nodes
//! chain together without an intervening call. A method-call chain
//! (`a.b().c().d()`, the fluent-builder pattern) is **not** flagged: each
//! call is a deliberate step the author chose to chain, which is a different
//! smell than reaching through unrelated objects' internals.
//!
//! Only the **outermost** node of a chain is reported (an inner
//! `a.b.c` within a longer `a.b.c.d.e` is not separately flagged), so one
//! overlong chain produces exactly one finding.

use tree_sitter::Node;

use crate::engine::{Diagnostic, Severity, Span};

const CHAIN_KIND_MARKERS: &[&str] = &[
    "member_expression",
    "field_expression",
    "attribute",
    "selector_expression",
];

/// Walk `root` and report every access chain deeper than `max_depth`.
pub fn scan(root: Node, max_depth: usize) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    walk(root, max_depth, &mut out);
    out
}

fn walk(node: Node, max_depth: usize, out: &mut Vec<Diagnostic>) {
    if is_chain_kind(node.kind()) && !is_chain_link(node) {
        // The outermost node of a chain: not itself part of a longer chain
        // (its parent is not also a chain-kind node).
        let depth = chain_depth(node);
        if depth > max_depth {
            out.push(diagnostic(node, depth));
        }
        // Do not descend further into this chain's own object expression —
        // an inner sub-chain of the same access would otherwise be reported
        // a second time. Still walk into non-chain children (call arguments,
        // index expressions) for chains elsewhere in the file.
        walk_non_chain_children(node, max_depth, out);
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk(cursor.node(), max_depth, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn walk_non_chain_children(node: Node, max_depth: usize, out: &mut Vec<Diagnostic>) {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if !is_chain_kind(child.kind()) {
                walk(child, max_depth, out);
            } else {
                walk_non_chain_children(child, max_depth, out);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Whether `node`'s parent is itself a chain-kind node with `node` as its
/// object/receiver — i.e. `node` is a link in a longer chain, not the
/// outermost access.
fn is_chain_link(node: Node) -> bool {
    node.parent().is_some_and(|parent| is_chain_kind(parent.kind()))
}

fn is_chain_kind(kind: &str) -> bool {
    let lower = kind.to_ascii_lowercase();
    CHAIN_KIND_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Count how many chain-kind nodes are nested along the "object" side of
/// `node`, i.e. its first named child, its first named child's first named
/// child, and so on.
fn chain_depth(node: Node) -> usize {
    let mut depth = 1;
    let mut current = node;
    while let Some(object) = current.named_child(0) {
        if is_chain_kind(object.kind()) {
            depth += 1;
            current = object;
        } else {
            break;
        }
    }
    depth
}

fn diagnostic(node: Node, depth: usize) -> Diagnostic {
    let start = node.start_position();
    let end = node.end_position();
    Diagnostic {
        engine: "quality".to_owned(),
        code: Some("law-of-demeter".to_owned()),
        severity: Severity::Warning,
        title: format!("access chain is {depth} levels deep"),
        description: Some("reach through fewer objects' internals, or add a helper method".to_owned()),
        span: Some(Span {
            start_line: start.row as u32 + 1,
            start_col: start.column as u32 + 1,
            end_line: end.row as u32 + 1,
            end_col: end.column as u32 + 1,
        }),
        url: None,
        fix: Vec::new(),
        metadata: std::collections::BTreeMap::new(),
    }
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

    #[test]
    fn flags_a_deep_attribute_chain() {
        let source = "a.b.c.d.e\n";
        let tree = parse("python", source);
        let findings = scan(tree.root_node(), 3);
        assert_eq!(findings.len(), 1, "expected exactly one finding for the whole chain");
        assert_eq!(findings[0].code.as_deref(), Some("law-of-demeter"));
    }

    #[test]
    fn does_not_flag_a_shallow_chain() {
        let source = "a.b.c\n";
        let tree = parse("python", source);
        assert!(scan(tree.root_node(), 3).is_empty());
    }

    #[test]
    fn does_not_flag_a_fluent_call_chain() {
        let source = "a.b().c().d().e()\n";
        let tree = parse("python", source);
        assert!(
            scan(tree.root_node(), 3).is_empty(),
            "call chains are a different smell"
        );
    }
}
