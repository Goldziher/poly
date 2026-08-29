//! Per-grammar node-kind classification shared by [`super::nesting`] and
//! [`super::complexity`].
//!
//! Both metrics walk the *whole* parse tree (not just the spans a Tags query
//! finds) because nesting must reset at an anonymous closure — a construct no
//! Tags query ever names — exactly as it resets at a named function. Keying
//! both walks off one table keeps the "what counts as a function boundary"
//! answer in exactly one place per grammar.
//!
//! # The nesting definition (reference doc §2)
//!
//! Measured **per function-like body**; the counter resets to 0 at every
//! function-like boundary (module top level is itself a body at 0). A
//! construct's depth is `1 + (counted enclosing constructs in the same
//! body)`. Strict `>`, so a default threshold of 4 fires at depth 5.
//!
//! - **+1** (`Increment` / `IncrementUnlessChained`): `if` (first in a chain
//!   only), loops, `switch`/`match`, `try`, conditional expressions.
//! - **+0** (`Same`): `else if`/`elif`, `else`, case/when/match arms,
//!   `catch`/`except`/`rescue`/`finally`, `with`/`using`/`defer`.
//! - **reset** (`Reset`): closures/lambdas/nested functions, class/struct/
//!   impl/module bodies.
//! - everything else: `Ignore` (no effect).

/// How a node kind affects the nesting-depth counter within its enclosing body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestRole {
    /// Always +1 relative to the enclosing body.
    Increment,
    /// +1 unless this node is the chained `else if`/`elif` continuation of a
    /// parent conditional — see `is_chained_else_if` in [`super::nesting`].
    IncrementUnlessChained,
    /// +0: continues the parent construct's depth (siblings of a branch).
    Same,
    /// Starts a brand new body, counting from 0 again.
    Reset,
    /// No effect on nesting.
    Ignore,
}

/// Whether a node counts as a McCabe decision point for
/// [`super::complexity`]. Chained `else if` and each `case`/`catch` arm all
/// count here even though they are `Same` for nesting purposes — the two
/// metrics measure different things from the same tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Construct {
    /// Nesting-depth role (see [`NestRole`]).
    pub nest: NestRole,
    /// Whether this node kind is itself a decision point for cyclomatic
    /// complexity (independent of the chain/else-if nesting exception).
    pub decision: bool,
    /// Whether this node kind's own text is a boolean short-circuit operator
    /// token (`&&`/`||`/`and`/`or`), each counted as a decision point.
    pub bool_operator: bool,
}

const IGNORE: Construct = Construct {
    nest: NestRole::Ignore,
    decision: false,
    bool_operator: false,
};

/// A construct that is +1 for nesting and a decision point for complexity
/// (subject to the else-if chain exception for nesting only).
const fn incr_chainable() -> Construct {
    Construct {
        nest: NestRole::IncrementUnlessChained,
        decision: true,
        bool_operator: false,
    }
}

/// A construct that is unconditionally +1 for nesting and a decision point.
const fn incr() -> Construct {
    Construct {
        nest: NestRole::Increment,
        decision: true,
        bool_operator: false,
    }
}

/// A construct that is +0 for nesting but still a decision point (case arms,
/// catch clauses, chained else-if wrappers).
const fn same_decision() -> Construct {
    Construct {
        nest: NestRole::Same,
        decision: true,
        bool_operator: false,
    }
}

/// A construct that is +0 for nesting and not a decision point (`else`,
/// `finally`, `with`, bare blocks, …).
const fn same() -> Construct {
    Construct {
        nest: NestRole::Same,
        decision: false,
        bool_operator: false,
    }
}

/// A function-like / container boundary: resets nesting to 0 and starts a
/// fresh cyclomatic-complexity scope.
const fn reset() -> Construct {
    Construct {
        nest: NestRole::Reset,
        decision: false,
        bool_operator: false,
    }
}

/// A boolean short-circuit operator token, counted once per occurrence for
/// complexity and otherwise inert.
const BOOL_OP: Construct = Construct {
    nest: NestRole::Ignore,
    decision: true,
    bool_operator: true,
};

/// Grammars with a [`classify`] table, used to gate whether nesting/complexity
/// run at all for a language (see the module docs on degrading honestly).
pub fn has_table(grammar: &str) -> bool {
    matches!(
        grammar,
        "python"
            | "rust"
            | "go"
            | "javascript"
            | "typescript"
            | "tsx"
            | "java"
            | "kotlin"
            | "c"
            | "cpp"
            | "csharp"
            | "ruby"
    )
}

/// Classify one node kind for one grammar. Returns [`IGNORE`] for any kind
/// this table does not name.
pub fn classify(grammar: &str, kind: &str) -> Construct {
    match grammar {
        "python" => classify_python(kind),
        "rust" => classify_rust(kind),
        "go" => classify_go(kind),
        "javascript" | "typescript" | "tsx" => classify_js_family(kind),
        "java" => classify_java(kind),
        "kotlin" => classify_kotlin(kind),
        "c" | "cpp" => classify_c_family(kind),
        "csharp" => classify_csharp(kind),
        "ruby" => classify_ruby(kind),
        _ => IGNORE,
    }
}

fn classify_python(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement" | "while_statement" | "try_statement" | "match_statement" | "conditional_expression" => incr(),
        "elif_clause" => same_decision(),
        "else_clause" | "except_clause" | "finally_clause" | "with_statement" | "case_clause" | "block" => same(),
        "boolean_operator" => BOOL_OP,
        "function_definition" | "lambda" | "class_definition" => reset(),
        _ => IGNORE,
    }
}

fn classify_rust(kind: &str) -> Construct {
    match kind {
        "if_expression" | "if_let_expression" => incr_chainable(),
        "for_expression" | "while_expression" | "while_let_expression" | "loop_expression" | "match_expression" => {
            incr()
        }
        "match_arm" | "block" => same(),
        "binary_expression" => IGNORE, // operator checked separately, see `is_bool_operator_text`.
        "closure_expression" | "function_item" | "impl_item" | "mod_item" | "trait_item" => reset(),
        _ => IGNORE,
    }
}

fn classify_go(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement" | "expression_switch_statement" | "type_switch_statement" | "select_statement" => incr(),
        "default_case" | "communication_case" | "expression_case" | "type_case" | "block" => same(),
        "function_declaration" | "method_declaration" | "func_literal" => reset(),
        _ => IGNORE,
    }
}

fn classify_js_family(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" | "switch_statement"
        | "try_statement" | "ternary_expression" => incr(),
        "switch_case" | "switch_default" | "catch_clause" | "finally_clause" | "statement_block" => same(),
        "function_declaration"
        | "function_expression"
        | "generator_function"
        | "generator_function_declaration"
        | "arrow_function"
        | "method_definition"
        | "class_declaration"
        | "class" => reset(),
        _ => IGNORE,
    }
}

fn classify_java(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement"
        | "enhanced_for_statement"
        | "while_statement"
        | "do_statement"
        | "switch_expression"
        | "switch_statement"
        | "try_statement"
        | "ternary_expression" => incr(),
        "switch_block_statement_group" | "switch_rule" | "catch_clause" | "finally_clause" | "block" => same(),
        "method_declaration"
        | "constructor_declaration"
        | "lambda_expression"
        | "class_declaration"
        | "interface_declaration"
        | "class_body" => reset(),
        _ => IGNORE,
    }
}

fn classify_kotlin(kind: &str) -> Construct {
    match kind {
        "if_expression" => incr_chainable(),
        "for_statement" | "while_statement" | "do_while_statement" | "when_expression" | "try_expression" => incr(),
        "when_entry" | "catch_block" | "finally_block" | "control_structure_body" => same(),
        "function_declaration" | "lambda_literal" | "anonymous_function" | "class_declaration" => reset(),
        _ => IGNORE,
    }
}

fn classify_c_family(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "try_statement"
        | "conditional_expression" => incr(),
        "else_clause" | "case_statement" | "catch_clause" | "compound_statement" => same(),
        "function_definition" | "lambda_expression" => reset(),
        _ => IGNORE,
    }
}

fn classify_csharp(kind: &str) -> Construct {
    match kind {
        "if_statement" => incr_chainable(),
        "for_statement"
        | "foreach_statement"
        | "while_statement"
        | "do_statement"
        | "switch_statement"
        | "switch_expression"
        | "try_statement"
        | "conditional_expression" => incr(),
        "switch_section" | "switch_expression_arm" | "catch_clause" | "finally_clause" | "block" => same(),
        "method_declaration"
        | "local_function_statement"
        | "lambda_expression"
        | "anonymous_method_expression"
        | "class_declaration" => reset(),
        _ => IGNORE,
    }
}

fn classify_ruby(kind: &str) -> Construct {
    match kind {
        "if" | "unless" => incr(),
        "for" | "while" | "until" | "case" | "begin" | "conditional" => incr(),
        "elsif" => same_decision(),
        "else" | "when" | "rescue" | "ensure" | "then" => same(),
        "method" | "class" | "module" | "block" | "lambda" | "do_block" => reset(),
        _ => IGNORE,
    }
}

/// Whether `text` is a boolean short-circuit operator, checked for languages
/// (Rust, C-family, JS, …) whose grammar folds `&&`/`||` into a generic
/// `binary_expression` rather than a dedicated node kind. Python's
/// `boolean_operator` and Ruby's `binary` are matched by kind instead (see
/// the per-grammar tables above), so this is only consulted for the
/// generic-binary-expression grammars.
pub fn is_bool_operator_text(text: &str) -> bool {
    matches!(text, "&&" | "||")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_table_covers_the_required_languages_only() {
        for grammar in [
            "python",
            "rust",
            "go",
            "javascript",
            "typescript",
            "tsx",
            "java",
            "kotlin",
            "c",
            "cpp",
            "csharp",
            "ruby",
        ] {
            assert!(has_table(grammar), "{grammar} should have a construct table");
        }
        for grammar in ["zig", "dart", "swift", "gleam", "elixir", "php", "nix"] {
            assert!(
                !has_table(grammar),
                "{grammar} intentionally has no construct table (degrades to line-counting only)"
            );
        }
    }

    #[test]
    fn python_elif_is_same_not_increment() {
        assert_eq!(classify_python("elif_clause").nest, NestRole::Same);
        assert_eq!(classify_python("if_statement").nest, NestRole::IncrementUnlessChained);
    }
}
