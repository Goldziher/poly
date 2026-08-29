//! `cyclomatic-complexity`: McCabe complexity per function-like scope, using
//! the same [`kinds`](super::kinds) construct table nesting uses.
//!
//! # Algorithm
//!
//! One recursive walk of the whole file. A stack of `(count, span)` tracks
//! the currently open function-like scopes (McCabe base complexity 1 per
//! scope). Entering a [`NestRole::Reset`] boundary pushes a new scope;
//! `construct.decision` nodes increment the innermost open scope's count
//! (chained `else if`, each `case`/`catch` arm, and unconditional loops all
//! count here — unlike nesting, chaining does not exempt a decision point).
//! Leaving a scope emits a finding when its final count exceeds the
//! threshold, using the boundary node's own span.
//!
//! Decisions outside any function-like scope (bare module-level code) are
//! discarded — cyclomatic complexity is conventionally a per-function metric,
//! and a file-level number would be new noise this tier does not need.
//!
//! `&&`/`||` are folded into a single generic `binary_expression` (or
//! `binary`, in Ruby) node by most of these grammars rather than getting a
//! dedicated node kind, so they are matched directly on the operator token's
//! own tree-sitter kind (`"&&"`/`"||"`, i.e. the literal punctuation as its
//! own anonymous node) rather than through the per-grammar table — this is
//! the same check for every language.

use tree_sitter::Node;

use super::kinds::{self, NestRole};

/// One cyclomatic-complexity finding for a single function-like scope.
pub struct ComplexityFinding {
    /// Final McCabe complexity of the scope (> threshold).
    pub complexity: usize,
    /// Byte range of the function-like boundary node, for the diagnostic span.
    pub start_byte: usize,
    /// See [`ComplexityFinding::start_byte`].
    pub end_byte: usize,
}

/// Walk `root` and collect every cyclomatic-complexity finding for `grammar`.
/// Returns an empty vec when `grammar` has no construct table.
pub fn analyze(grammar: &str, root: Node, threshold: usize) -> Vec<ComplexityFinding> {
    let mut findings = Vec::new();
    if !kinds::has_table(grammar) {
        return findings;
    }
    let mut scopes: Vec<Scope> = Vec::new();
    walk(root, grammar, &mut scopes, threshold, &mut findings);
    findings
}

struct Scope {
    count: usize,
    start_byte: usize,
    end_byte: usize,
}

fn walk(node: Node, grammar: &str, scopes: &mut Vec<Scope>, threshold: usize, findings: &mut Vec<ComplexityFinding>) {
    let construct = kinds::classify(grammar, node.kind());
    let opened_scope = construct.nest == NestRole::Reset;
    if opened_scope {
        scopes.push(Scope {
            count: 1,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        });
    }

    if construct.decision {
        bump(scopes);
    }
    if has_bool_operator_child(node) {
        bump(scopes);
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk(cursor.node(), grammar, scopes, threshold, findings);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    if opened_scope {
        let scope = scopes.pop().expect("scope pushed above");
        if scope.count > threshold {
            findings.push(ComplexityFinding {
                complexity: scope.count,
                start_byte: scope.start_byte,
                end_byte: scope.end_byte,
            });
        }
    }
}

fn bump(scopes: &mut [Scope]) {
    if let Some(top) = scopes.last_mut() {
        top.count += 1;
    }
}

/// Whether `node` has an immediate child whose own kind is the literal `&&`
/// or `||` punctuation token — how tree-sitter represents these operators
/// inside a generic binary-expression node across every grammar in the
/// construct table (Rust, C, C++, C#, Java, Kotlin, Go, JS/TS, Ruby all fold
/// logical-and/or into one generic binary node rather than a dedicated kind).
fn has_bool_operator_child(node: Node) -> bool {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        if kinds::is_bool_operator_text(cursor.node().kind()) {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
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
    fn simple_function_has_base_complexity_one() {
        let tree = parse("rust", "fn f(a: i32) -> i32 {\n    a + 1\n}\n");
        assert!(
            analyze("rust", tree.root_node(), 1).is_empty(),
            "complexity 1 does not exceed threshold 1"
        );
    }

    #[test]
    fn each_branch_and_bool_operator_adds_one() {
        // 1 (base) + if + else-if + && + || = 5.
        let source = "fn f(a: i32, b: i32) -> i32 {\n    if a > 0 && b > 0 {\n        1\n    } else if a < 0 || b < 0 {\n        2\n    } else {\n        3\n    }\n}\n";
        let tree = parse("rust", source);
        let findings = analyze("rust", tree.root_node(), 4);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].complexity, 5);
    }

    #[test]
    fn nested_closure_gets_its_own_independent_scope() {
        let source = "fn f(a: i32) -> i32 {\n    if a > 0 {\n        let g = |x: i32| if x > 0 { x } else { -x };\n        g(a)\n    } else {\n        0\n    }\n}\n";
        let tree = parse("rust", source);
        // Outer: base 1 + if = 2 (below any reasonable threshold).
        // Closure: base 1 + if = 2, independently scoped.
        assert!(
            analyze("rust", tree.root_node(), 1).len() == 2,
            "both scopes exceed threshold 1"
        );
        assert!(
            analyze("rust", tree.root_node(), 2).is_empty(),
            "neither scope exceeds threshold 2"
        );
    }

    #[test]
    fn languages_without_a_construct_table_report_nothing() {
        let tree = parse(
            "php",
            "<?php\nfunction f($a) {\n  if ($a && $a > 0) {\n    return 1;\n  }\n  return 0;\n}\n",
        );
        assert!(analyze("php", tree.root_node(), 1).is_empty());
    }
}
