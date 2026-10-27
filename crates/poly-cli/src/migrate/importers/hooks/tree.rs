//! Lefthook's job tree and its flattening into poly's flat stage.
//!
//! Lefthook nests groups, each sequential (the default), `parallel` or `piped`.
//! A poly stage is flat, but jobs carry a `priority`: lower priorities finish
//! before higher ones start, and jobs sharing one run together when the stage is
//! `parallel`. Assigning each leaf the earliest priority its position allows
//! reproduces the ordering exactly, at the cost of a barrier wherever one
//! branch of a parallel group is longer than its siblings.

/// How a group runs its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Sequential,
    Parallel,
    Piped,
}

/// A node of the job tree.
#[derive(Debug, Clone)]
pub(super) enum Node<T> {
    Leaf(T),
    Group { mode: Mode, children: Vec<Node<T>> },
}

/// A leaf with the priority layer it was placed on.
#[derive(Debug, Clone)]
pub(super) struct Placed<T> {
    pub leaf: T,
    pub layer: i64,
}

/// Flatten `node` into leaves tagged with their layer, in depth-first order.
pub(super) fn layout<T>(node: Node<T>) -> Vec<Placed<T>> {
    let mut out = Vec::new();
    place(node, 0, &mut out);
    out
}

/// Place `node` starting at layer `start`; returns how many layers it spans.
fn place<T>(node: Node<T>, start: i64, out: &mut Vec<Placed<T>>) -> i64 {
    match node {
        Node::Leaf(leaf) => {
            out.push(Placed { leaf, layer: start });
            1
        }
        Node::Group { mode, children } => {
            let mut next = start;
            let mut span = 0;
            for child in children {
                let child_span = match mode {
                    Mode::Parallel => place(child, start, out),
                    Mode::Sequential | Mode::Piped => place(child, next, out),
                };
                match mode {
                    Mode::Parallel => span = span.max(child_span),
                    Mode::Sequential | Mode::Piped => {
                        next += child_span;
                        span = next - start;
                    }
                }
            }
            span
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(name: &'static str) -> Node<&'static str> {
        Node::Leaf(name)
    }

    fn group(mode: Mode, children: Vec<Node<&'static str>>) -> Node<&'static str> {
        Node::Group { mode, children }
    }

    fn layers(node: Node<&'static str>) -> Vec<(&'static str, i64)> {
        layout(node).into_iter().map(|p| (p.leaf, p.layer)).collect()
    }

    #[test]
    fn sequential_children_take_successive_layers() {
        assert_eq!(
            layers(group(Mode::Sequential, vec![leaf("a"), leaf("b"), leaf("c")])),
            [("a", 0), ("b", 1), ("c", 2)]
        );
    }

    #[test]
    fn parallel_children_share_a_layer() {
        assert_eq!(
            layers(group(Mode::Parallel, vec![leaf("a"), leaf("b")])),
            [("a", 0), ("b", 0)]
        );
    }

    #[test]
    fn armis_shape_fix_then_check_with_parallel_chains() {
        let tree = group(
            Mode::Sequential,
            vec![
                group(
                    Mode::Sequential,
                    vec![
                        group(
                            Mode::Parallel,
                            vec![
                                group(Mode::Sequential, vec![leaf("ruff-format"), leaf("ruff-check")]),
                                group(Mode::Sequential, vec![leaf("oxlint"), leaf("oxfmt")]),
                                leaf("go-format"),
                            ],
                        ),
                        leaf("pyproject-format"),
                    ],
                ),
                group(Mode::Parallel, vec![leaf("go-lint"), leaf("actionlint")]),
            ],
        );
        assert_eq!(
            layers(tree),
            [
                ("ruff-format", 0),
                ("ruff-check", 1),
                ("oxlint", 0),
                ("oxfmt", 1),
                ("go-format", 0),
                ("pyproject-format", 2),
                ("go-lint", 3),
                ("actionlint", 3),
            ]
        );
    }

    #[test]
    fn empty_groups_take_no_layer() {
        assert_eq!(
            layers(group(Mode::Sequential, vec![group(Mode::Parallel, vec![]), leaf("a")])),
            [("a", 0)]
        );
    }
}
