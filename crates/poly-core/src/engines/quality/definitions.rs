//! Tags-query-driven structural definitions for `function-too-long`,
//! `type-too-long`, and `too-many-parameters`.
//!
//! # Built-in query table
//!
//! `tree-sitter-language-pack`'s bundled `tags.scm` is usable as-is for most
//! grammars, but three gaps were found by probing it directly (not assumed
//! from the reference doc's OTHER-family feasibility matrix, which never
//! covered these):
//!
//! - **TypeScript and TSX's bundled tags query is signature-only.** It
//!   captures `function_signature`/`method_signature` (ambient overloads,
//!   never a real function body) and `abstract_class_declaration` (never a
//!   concrete class) — a plain `function foo() { … }` or `class Bar { … }`
//!   produces **zero** captures. [`BUILTIN_TAGS`] below supplies a minimal
//!   replacement query naming the real node kinds directly.
//! - **Zig has no bundled tags query at all** (`get_tags_query` returns
//!   `None`). Zig's grammar wraps a function's signature (`FnProto`) and body
//!   (`Block`) as two *siblings* under one `Decl` node, so `(Decl (FnProto))
//!   @definition.function` captures the whole function in one shot.
//! - **JSX has no grammar of its own.** `detect_language("*.jsx")` resolves
//!   to `"javascript"` (confirmed empirically), so `.jsx` files never reach
//!   this table at all — they parse and tag exactly like `.js`.
//!
//! # Per-language quirks handled inline
//!
//! - **C and C++** capture `function_declarator` — the signature only, one
//!   line. [`fix_span`] walks one hop up to the enclosing `function_definition`
//!   to recover the real span; the *original* captured node is kept for
//!   parameter counting, since the parameter list is its child, not the
//!   definition's.
//! - **Dart** captures `method_signature`/`function_signature`, whose actual
//!   function body (`function_body`) is a *sibling*, not a descendant —
//!   `fix_span` unions the two when the immediately following sibling is a
//!   `function_body`.
//! - **Swift**'s bundled query fires `@definition.method` on the *class*
//!   declaration itself (not the method) — excluded explicitly in
//!   [`is_known_bad_capture`].
//! - **Gleam** emits `@definition.constructor` once per enum variant, each
//!   spanning the *whole* type — never whitelisted as type-like, so it is
//!   silently ignored by construction (see [`classify_capture`]).
//! - Every capture is deduplicated by `(start_byte, end_byte)` after fixups,
//!   which absorbs Dart emitting the same method three times over and Rust
//!   emitting `@definition.method` and `@definition.function` on one node
//!   for inherent-impl methods.
//!
//! # Zero-match guard
//!
//! A query that matches nothing in a non-empty file means "no structural
//! model of this file", not "this file is clean" — callers must fall back to
//! line-counting rules rather than reporting zero definitions as success
//! (mirrors `treesitter/indent.rs:126-128`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};
use tree_sitter_language_pack::get_tags_query;

/// Built-in tags-shaped queries for grammars whose bundled query is missing
/// or unusable for structural metrics. Modelled on `BUILTIN_QUERIES` in
/// `engines/treesitter/indent.rs`. Consulted *before* the bundled query.
static BUILTIN_TAGS: &[(&str, &str)] = &[
    ("typescript", TS_FAMILY_TAGS),
    ("tsx", TS_FAMILY_TAGS),
    ("zig", ZIG_TAGS),
];

/// Replacement for TypeScript/TSX's signature-only bundled query, naming the
/// real declaration/definition node kinds directly (see module docs).
const TS_FAMILY_TAGS: &str = "\
(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method
(interface_declaration name: (type_identifier) @name) @definition.interface
";

/// Zig has no bundled tags query. A function's signature (`FnProto`) and body
/// (`Block`) are siblings under one `Decl` node, so capturing `Decl` whenever
/// it has a `FnProto` child gives the whole function in one span.
const ZIG_TAGS: &str = "(Decl (FnProto)) @definition.function";

thread_local! {
    static QUERY_STATE: RefCell<HashMap<String, (QueryCursor, Query)>> = RefCell::new(HashMap::new());
}

/// Whether `grammar` has any Tags/built-in query at all, without compiling or
/// running it. Used by callers to decide whether to claim structural
/// coverage before attempting it.
pub fn has_query(grammar: &str) -> bool {
    BUILTIN_TAGS.iter().any(|(name, _)| *name == grammar) || get_tags_query(grammar).is_some()
}

/// A `function-like` or `type-like` structural definition found in a file.
pub struct Definition {
    /// Whether this is a function/method or a type (class/struct/interface/…).
    pub kind: DefKind,
    /// 0-based start row of the (fixed-up) span.
    pub start_row: usize,
    /// 0-based end row of the (fixed-up) span.
    pub end_row: usize,
    /// Byte offset of the span start, for the diagnostic.
    pub start_byte: usize,
    /// Byte offset of the span end, for the diagnostic.
    pub end_byte: usize,
    /// Number of parameters, when a parameter list could be located.
    /// `None` for type-like definitions and for a function-like definition
    /// whose signature carries no locatable parameter list.
    pub param_count: Option<usize>,
}

/// What a [`Definition`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    /// A function or method.
    Function,
    /// A class, struct, interface, or similar type.
    Type,
}

/// Collect every structural definition in `root`, applying the per-language
/// fixups and quirk exclusions documented above. Returns `None` when
/// `grammar` has no usable query at all (caller should fall back to
/// line-counting rules), and `Some(vec![])` only when the query genuinely
/// matched nothing productive (still a fallback signal — see the zero-match
/// guard in the module docs, applied by the caller).
pub fn collect(grammar: &str, root: Node, source: &[u8]) -> Option<Vec<Definition>> {
    let query_src = BUILTIN_TAGS
        .iter()
        .find(|(name, _)| *name == grammar)
        .map(|(_, q)| *q)
        .or_else(|| get_tags_query(grammar))?;

    QUERY_STATE.with(|cell| {
        let mut pool = cell.borrow_mut();
        if !pool.contains_key(grammar) {
            let language = tree_sitter_language_pack::get_language(grammar).ok()?;
            let query = Query::new(&language, query_src).ok()?;
            pool.insert(grammar.to_owned(), (QueryCursor::new(), query));
        }
        let (cursor, query) = pool.get_mut(grammar)?;
        Some(run(grammar, query, cursor, root, source))
    })
}

/// Run the compiled Tags query and build the deduplicated, fixed-up
/// definition list. Split out of [`collect`] so the thread-local borrow of
/// the parser/cursor/query triple stays scoped to one `with` call.
fn run(grammar: &str, query: &Query, cursor: &mut QueryCursor, root: Node, source: &[u8]) -> Vec<Definition> {
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();

    let mut matches = cursor.matches(query, root, source);
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            let Some(def_kind) = classify_capture(capture_name) else {
                continue;
            };
            let node = capture.node;
            if is_known_bad_capture(grammar, capture_name, node.kind()) {
                continue;
            }
            let span = fix_span(grammar, node);
            let key = (span.start_byte, span.end_byte);
            if !seen.insert(key) {
                continue;
            }
            let param_count = match def_kind {
                DefKind::Function => find_parameter_list(node).map(count_parameters),
                DefKind::Type => None,
            };
            out.push(Definition {
                kind: def_kind,
                start_row: span.start_row,
                end_row: span.end_row,
                start_byte: span.start_byte,
                end_byte: span.end_byte,
                param_count,
            });
        }
    }
    out
}

/// Map a Tags capture name to a [`DefKind`], whitelisting only the capture
/// families this engine measures. Every other capture — `@name`,
/// `@reference.*`, `@definition.constant`/`.property`/`.field`/`.module`/
/// `.object`/`.constructor`/`.variable` — is silently ignored, which is what
/// keeps Gleam's per-variant `@definition.constructor` spam and Kotlin's
/// function-local `@definition.constant` out of this engine without a
/// per-language exclusion list for each.
fn classify_capture(capture_name: &str) -> Option<DefKind> {
    match capture_name {
        "definition.function" | "definition.method" => Some(DefKind::Function),
        "definition.class"
        | "definition.interface"
        | "definition.struct"
        | "definition.type"
        | "definition.trait"
        | "definition.enum" => Some(DefKind::Type),
        _ => None,
    }
}

/// Named, per-language exclusions for a capture that is syntactically
/// well-formed but semantically wrong (see the module docs for why each
/// exists).
fn is_known_bad_capture(grammar: &str, capture_name: &str, node_kind: &str) -> bool {
    // Swift's bundled query fires `@definition.method` on the *class*
    // declaration itself, not on the method — every class would otherwise
    // measure as a "method" spanning the whole class body.
    grammar == "swift" && capture_name == "definition.method" && node_kind == "class_declaration"
}

/// The byte/row bounds of a fixed-up definition span. A plain struct rather
/// than a `Node`, because the Dart fixup unions two sibling nodes' ranges —
/// something no single real `Node` can represent.
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
}

impl Span {
    fn of(node: Node) -> Span {
        Span {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: node.start_position().row,
            end_row: node.end_position().row,
        }
    }
}

/// Apply the per-language span fixups documented in the module docs.
/// Parameter counting always searches from the *original* captured `node`
/// (passed separately by the caller), since the C/C++ walk-up target
/// (`function_definition`) no longer directly holds the parameter list the
/// way `function_declarator` did.
fn fix_span(grammar: &str, node: Node) -> Span {
    if matches!(grammar, "c" | "cpp") && node.kind() == "function_declarator" {
        let mut cursor = node;
        for _ in 0..4 {
            match cursor.parent() {
                Some(parent) if parent.kind() == "function_definition" => return Span::of(parent),
                Some(parent) => cursor = parent,
                None => break,
            }
        }
        return Span::of(node);
    }
    if grammar == "dart"
        && matches!(node.kind(), "method_signature" | "function_signature")
        && let Some(sibling) = node.next_sibling()
        && sibling.kind() == "function_body"
    {
        return Span {
            start_byte: node.start_byte(),
            end_byte: sibling.end_byte(),
            start_row: node.start_position().row,
            end_row: sibling.end_position().row,
        };
    }
    Span::of(node)
}

/// Bounded-depth search for a parameter-list node within `node`'s subtree,
/// without descending into a function body (so a nested closure's own
/// parameter list is never mistaken for the outer signature's). Matches a
/// small allowlist of known parameter-list kind names rather than a
/// substring test, so Rust's `type_parameters` (generics) is never confused
/// with `parameters` (values).
const PARAM_LIST_KINDS: &[&str] = &[
    "parameters",
    "formal_parameters",
    "parameter_list",
    "method_parameters",
    "lambda_parameters",
    "ParamDeclList",
];

const BODY_KINDS: &[&str] = &[
    "block",
    "compound_statement",
    "statement_block",
    "function_body",
    "Block",
];

fn find_parameter_list(node: Node) -> Option<Node> {
    find_parameter_list_bounded(node, 4)
}

fn find_parameter_list_bounded(node: Node, depth: usize) -> Option<Node> {
    if depth == 0 {
        return None;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        if PARAM_LIST_KINDS.contains(&child.kind()) {
            return Some(child);
        }
        if !BODY_KINDS.contains(&child.kind())
            && let Some(found) = find_parameter_list_bounded(child, depth - 1)
        {
            return Some(found);
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

/// Count value parameters in a located parameter-list node, excluding
/// receiver parameters (`self`/`this`) so a method's arity is not inflated by
/// the implicit receiver.
fn count_parameters(list: Node) -> usize {
    let mut count = 0usize;
    let mut cursor = list.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() && !child.kind().to_ascii_lowercase().contains("self") {
                count += names_in(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    count
}

/// How many parameters one parameter-list entry declares. Almost every
/// grammar's parameter node is one parameter (returns 1), but Go groups
/// same-typed parameters into a single `parameter_declaration` carrying
/// multiple `name`-field identifiers (`a, b, c int`) — counting the node
/// itself as one parameter there would silently undercount every grouped
/// signature. Counting `name`-field children generalizes correctly to both
/// shapes without a per-grammar special case.
fn names_in(node: Node) -> usize {
    let mut names = 0u32;
    for index in 0..node.child_count() as u32 {
        if node.field_name_for_child(index) == Some("name") {
            names += 1;
        }
    }
    names.max(1) as usize
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

    /// The exact failure family named in the task: without the C/C++
    /// parent-walk, `function_declarator` measures as 1 line and a genuinely
    /// long C function never fires `function-too-long`.
    #[test]
    fn c_function_declarator_is_walked_up_to_the_full_definition() {
        let mut body = String::from("int add(int a, int b) {\n");
        for i in 0..40 {
            body.push_str(&format!("    int t{i} = a + b + {i};\n"));
        }
        body.push_str("    return a + b;\n}\n");
        let tree = parse("c", &body);
        let source = body.as_bytes();
        let defs = collect("c", tree.root_node(), source).expect("c has a tags query");
        assert_eq!(defs.len(), 1, "expected exactly one function definition");
        let function_lines = defs[0].end_row - defs[0].start_row + 1;
        assert!(
            function_lines > 40,
            "expected the full function span (>40 lines), got {function_lines} — \
             the function_declarator parent-walk regressed"
        );
        assert_eq!(defs[0].param_count, Some(2));
    }

    #[test]
    fn c_short_function_is_not_flagged_as_long() {
        let tree = parse("c", "int add(int a, int b) {\n    return a + b;\n}\n");
        let source = b"int add(int a, int b) {\n    return a + b;\n}\n";
        let defs = collect("c", tree.root_node(), source).expect("c has a tags query");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].end_row - defs[0].start_row + 1, 3);
    }

    #[test]
    fn typescript_builtin_query_finds_real_functions_and_classes() {
        let source = "function foo(a: number, b: number): number {\n  return a + b;\n}\n\nclass Bar {\n  baz(x: number) {\n    return x;\n  }\n}\n";
        let tree = parse("typescript", source);
        let defs = collect("typescript", tree.root_node(), source.as_bytes()).expect("built-in query present");
        let functions = defs.iter().filter(|d| d.kind == DefKind::Function).count();
        let types = defs.iter().filter(|d| d.kind == DefKind::Type).count();
        assert_eq!(functions, 2, "expected `foo` and `baz`");
        assert_eq!(types, 1, "expected `Bar`");
    }

    #[test]
    fn zig_builtin_query_captures_the_whole_function() {
        let source = "fn foo(a: i32, b: i32) i32 {\n    return a + b;\n}\n";
        let tree = parse("zig", source);
        let defs = collect("zig", tree.root_node(), source.as_bytes()).expect("built-in zig query present");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].end_row - defs[0].start_row + 1, 3);
    }

    #[test]
    fn swift_class_is_never_misclassified_as_a_method() {
        let source = "class Bar {\n    func baz(a: Int) {\n        let x = 1\n    }\n}\n";
        let tree = parse("swift", source);
        let defs = collect("swift", tree.root_node(), source.as_bytes()).expect("swift has a tags query");
        // Exactly one function (`baz`) and one type (`Bar`) — the bad
        // `@definition.method` capture on `class_declaration` is excluded.
        assert_eq!(defs.iter().filter(|d| d.kind == DefKind::Function).count(), 1);
        assert_eq!(defs.iter().filter(|d| d.kind == DefKind::Type).count(), 1);
    }

    #[test]
    fn rust_impl_method_is_not_double_counted() {
        let source = "impl Bar {\n    fn baz(&self) {}\n}\n";
        let tree = parse("rust", source);
        let defs = collect("rust", tree.root_node(), source.as_bytes()).expect("rust has a tags query");
        assert_eq!(
            defs.len(),
            1,
            "rust tags emits both @definition.method and @definition.function for one node"
        );
    }

    #[test]
    fn zero_match_on_a_non_empty_file_is_distinguishable_from_none() {
        // A file with a tags-capable grammar but no matching construct at all
        // (bare statements) should yield `Some(vec![])`, not `None` — the
        // caller can then legitimately treat it as "no functions here" rather
        // than "no structural model of this language".
        let tree = parse("rust", "static X: i32 = 1;\n");
        let defs = collect("rust", tree.root_node(), b"static X: i32 = 1;\n");
        assert_eq!(defs.map(|d| d.len()), Some(0));
    }

    /// Only `"csharp"` resolves in the language pack — `"c_sharp"` is not a
    /// real grammar name and has no bundled or built-in query, which
    /// `has_query` must report honestly (a hand-written table keyed on a
    /// guessed string would otherwise silently lose C# coverage).
    #[test]
    fn misspelled_grammar_name_has_no_query() {
        assert!(!has_query("c_sharp"));
        assert!(has_query("csharp"));
    }
}
