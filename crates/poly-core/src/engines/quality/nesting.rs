//! `nesting-too-deep`: one definition, applied identically across every
//! grammar with a [`kinds`](super::kinds) construct table. See
//! `super::kinds` module docs for the exact +1/+0/reset rules.
//!
//! # Algorithm
//!
//! One recursive walk of the **whole file's parse tree** (not scoped to Tags
//! definitions — an anonymous closure resets nesting exactly like a named
//! function, and no Tags query ever names a closure). A running `depth`
//! counter is threaded through the walk, reset to 0 whenever a
//! [`NestRole::Reset`] node boundary is entered. Every node whose own
//! computed depth exceeds the threshold is reported once, at that node —
//! matching oxlint's `max-depth` behavior (which is why JS/TS defers to it):
//! a chain that reaches depth 5 with a threshold of 4 yields exactly one
//! finding, at the depth-5 node, because depths 1-4 do not exceed 4.

use tree_sitter::Node;

use super::kinds::{self, NestRole};

/// One nesting-too-deep finding: a construct whose depth exceeds the
/// configured threshold.
pub struct NestingFinding {
    /// The depth reached at this construct (> threshold).
    pub depth: usize,
    /// Byte range of the offending construct, for the diagnostic span.
    pub start_byte: usize,
    /// See [`NestingFinding::start_byte`].
    pub end_byte: usize,
}

/// Walk `root` and collect every nesting-too-deep finding for `grammar`.
/// Returns an empty vec (not an error) when `grammar` has no construct table
/// — the caller is responsible for not claiming coverage in that case.
pub fn analyze(grammar: &str, root: Node, threshold: usize) -> Vec<NestingFinding> {
    let mut findings = Vec::new();
    if !kinds::has_table(grammar) {
        return findings;
    }
    walk(root, grammar, 0, threshold, &mut findings);
    findings
}

/// Compute the maximum depth reached anywhere in `root`'s tree, for
/// [`super::kinds`]-table-driven grammars. Test-only helper: the production
/// path never needs "the max", only "does this node exceed the threshold"
/// (see module docs), but the 4-language fixture asserts the exact number.
#[cfg(test)]
pub fn max_depth(grammar: &str, root: Node) -> usize {
    let mut max = 0usize;
    max_depth_walk(root, grammar, 0, &mut max);
    max
}

#[cfg(test)]
fn max_depth_walk(node: Node, grammar: &str, depth: usize, max: &mut usize) {
    let construct = kinds::classify(grammar, node.kind());
    let next_depth = match construct.nest {
        NestRole::Increment => depth + 1,
        NestRole::IncrementUnlessChained => {
            if is_chained_else_if(node, grammar) {
                depth
            } else {
                depth + 1
            }
        }
        NestRole::Same | NestRole::Ignore => depth,
        NestRole::Reset => 0,
    };
    *max = (*max).max(next_depth);
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            max_depth_walk(cursor.node(), grammar, next_depth, max);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

fn walk(node: Node, grammar: &str, depth: usize, threshold: usize, findings: &mut Vec<NestingFinding>) {
    let construct = kinds::classify(grammar, node.kind());
    let next_depth = match construct.nest {
        NestRole::Increment => depth + 1,
        NestRole::IncrementUnlessChained => {
            if is_chained_else_if(node, grammar) {
                depth
            } else {
                depth + 1
            }
        }
        NestRole::Same | NestRole::Ignore => depth,
        NestRole::Reset => 0,
    };

    if next_depth > threshold && next_depth > depth {
        findings.push(NestingFinding {
            depth: next_depth,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        });
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            walk(cursor.node(), grammar, next_depth, threshold, findings);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Whether `node` (already classified as `IncrementUnlessChained`, i.e. an
/// `if`-like node) is the `else if`/`elif` continuation of a parent
/// conditional rather than a freshly opened branch.
///
/// Field/wrapper conventions differ per grammar family (verified empirically
/// against each grammar, not assumed from convention):
/// - Java, C#, Go: the nested `if` is directly the parent if-node's
///   `alternative` field child.
/// - C, C++, Rust, JavaScript, TypeScript, TSX: the `alternative` field is an
///   intermediate `else_clause` node whose own child is the nested if-node,
///   so the immediate parent of the chained if is `else_clause`, not the
///   outer if-node itself.
/// - Kotlin: the `alternative` field child is a `control_structure_body`
///   wrapper, and the chained `if_expression` is that wrapper's child.
/// - Python and Ruby never reach this function at all (`elif_clause`/`elsif`
///   are distinct sibling node kinds classified directly in the construct
///   table, with no nested if-node to disambiguate).
fn is_chained_else_if(node: Node, grammar: &str) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match grammar {
        "c" | "cpp" | "rust" | "javascript" | "typescript" | "tsx" => parent.kind() == "else_clause",
        "kotlin" => {
            parent.kind() == "control_structure_body"
                && parent
                    .parent()
                    .is_some_and(|grandparent| is_alternative_of(&grandparent, &parent))
        }
        "java" | "csharp" | "go" => is_alternative_of(&parent, &node),
        _ => false,
    }
}

/// Whether `child` is exactly `parent`'s `alternative` field value.
fn is_alternative_of(parent: &Node, child: &Node) -> bool {
    parent
        .child_by_field_name("alternative")
        .is_some_and(|alt| alt.id() == child.id())
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

    /// The 4-language fixture from the reference doc §2, verbatim: structurally
    /// identical Python/Rust/Go/TypeScript snippets, each `for`(1) -> `if`(2,
    /// with an `else if` that must not add) -> `while`(3) -> `switch`/`match`(4)
    /// -> inner `if`(5), each holding a closure whose own `if` scores 1.
    ///
    /// This is the proof the metric is language-independent: all four must
    /// compute max depth 5 and exactly one finding at threshold 4.
    #[test]
    fn four_language_fixture_all_reach_depth_five() {
        let python = r#"
def outer(items):
    for item in items:
        if item > 0:
            while item > 1:
                match item:
                    case 1:
                        if item == 1:
                            pass
        elif item < 0:
            if item < -1:
                if item < -2:
                    pass
    callback = lambda x: x if x > 0 else -x
"#;
        let rust = r#"
fn outer(items: &[i32]) {
    for item in items {
        if *item > 0 {
            while *item > 1 {
                match item {
                    1 => {
                        if *item == 1 {
                        }
                    }
                    _ => {}
                }
                break;
            }
        } else if *item < 0 {
            if *item < -1 {
                if *item < -2 {
                }
            }
        }
    }
    let callback = |x: i32| if x > 0 { x } else { -x };
}
"#;
        let go = r#"
package main

func outer(items []int) {
	for _, item := range items {
		if item > 0 {
			for item > 1 {
				switch item {
				case 1:
					if item == 1 {
					}
				}
				break
			}
		} else if item < 0 {
			if item < -1 {
				if item < -2 {
				}
			}
		}
	}
	callback := func(x int) int {
		if x > 0 {
			return x
		}
		return -x
	}
	_ = callback
}
"#;
        let typescript = r#"
function outer(items: number[]) {
  for (const item of items) {
    if (item > 0) {
      while (item > 1) {
        switch (item) {
          case 1:
            if (item === 1) {
            }
            break;
        }
        break;
      }
    } else if (item < 0) {
      if (item < -1) {
        if (item < -2) {
        }
      }
    }
  }
  const callback = (x: number) => (x > 0 ? x : -x);
}
"#;

        for (grammar, source) in [
            ("python", python),
            ("rust", rust),
            ("go", go),
            ("typescript", typescript),
        ] {
            let tree = parse(grammar, source);
            let depth = max_depth(grammar, tree.root_node());
            assert_eq!(depth, 5, "{grammar}: expected max nesting depth 5");

            let findings = analyze(grammar, tree.root_node(), 4);
            assert_eq!(
                findings.len(),
                1,
                "{grammar}: expected exactly one finding at threshold 4, got {}",
                findings.len()
            );
            assert_eq!(findings[0].depth, 5, "{grammar}: the one finding must be at depth 5");
        }
    }

    #[test]
    fn closure_body_does_not_inherit_outer_depth() {
        // The closure's own `if` must score depth 1, not 6 — it resets.
        let rust = "fn f(v: &[i32]) {\n    for a in v {\n        if *a > 0 {\n            if *a > 1 {\n                if *a > 2 {\n                    let g = |x: i32| if x > 0 { x } else { 0 };\n                }\n            }\n        }\n    }\n}\n";
        let tree = parse("rust", rust);
        // Outer chain: for(1) if(2) if(3) if(4) -> max depth 4, no finding at
        // threshold 4 (needs strict >).
        assert_eq!(max_depth("rust", tree.root_node()), 4);
        assert!(analyze("rust", tree.root_node(), 4).is_empty());
    }

    #[test]
    fn languages_without_a_construct_table_report_nothing() {
        let tree = parse(
            "php",
            "<?php\nif ($x) {\n  if ($y) {\n    if ($z) {\n      if ($w) {\n        echo 1;\n      }\n    }\n  }\n}\n",
        );
        assert!(analyze("php", tree.root_node(), 4).is_empty());
    }
}
