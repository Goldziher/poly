use std::path::PathBuf;

use super::*;
use crate::config::GlobalDefaults;

fn cfg(indent_width: usize) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width,
        options: toml::Table::new(),
    }
}

fn src(path: &str, language: Language, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language,
        content: content.into(),
    }
}

#[test]
fn metadata_is_format_only() {
    let engine = TreeSitterEngine;
    assert_eq!(engine.name(), "treesitter");
    assert!(engine.languages().is_empty());
    let caps = engine.capabilities();
    assert!(caps.format);
    // The generic tier is a formatter only — trailing-whitespace normalization
    // is a `fmt` concern, never surfaced as a `lint` diagnostic.
    assert!(!caps.lint);
}

fn formatted_text(out: FormatOutput, original: &str) -> String {
    match out {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => original.to_string(),
    }
}

#[test]
fn rust_raw_string_interior_is_byte_preserved_while_code_reindents() {
    // Raw string interior (including `{`) is verbatim; surrounding code reindents.
    let engine = TreeSitterEngine;
    let input = concat!(
        "fn main() {\n",
        "let template = r#\"\n",
        "        deeply indented {line}\n",
        "   another\n",
        "\"#;\n",
        "println!(\"{}\", template);\n",
        "}\n",
    );
    let expected = concat!(
        "fn main() {\n",
        "    let template = r#\"\n",
        "        deeply indented {line}\n",
        "   another\n",
        "\"#;\n",
        "    println!(\"{}\", template);\n",
        "}\n",
    );
    let s = src("main.rs", Language::Other("rust".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "code reindented, string interior preserved");
    // The exact interior bytes between the raw-string delimiters survive.
    let interior = "\n        deeply indented {line}\n   another\n";
    assert!(text.contains(interior), "raw-string interior must be verbatim");
}

#[test]
fn go_reindents_with_tabs_not_spaces() {
    let engine = TreeSitterEngine;
    let input = concat!("package main\n", "\n", "func main() {\n", "x := 1\n", "}\n");
    let expected = concat!("package main\n", "\n", "func main() {\n", "\tx := 1\n", "}\n",);
    let s = src("main.go", Language::Other("go".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Go must reindent with a tab, not spaces");
}

#[test]
fn whitespace_fallback_for_unknown_language() {
    // An unknown grammar name is never in BRACE_FAMILY, so the engine only
    // normalizes whitespace — no parsing, no grammar download.
    let engine = TreeSitterEngine;
    let s = src(
        "notes.unknownext",
        Language::Other("definitely-not-a-grammar".into()),
        "line with trailing spaces   \nok\n",
    );
    let out = engine.format(&s, &cfg(2)).unwrap();
    match out {
        FormatOutput::Formatted(text) => {
            assert_eq!(text, "line with trailing spaces\nok\n");
        }
        FormatOutput::Unchanged => panic!("expected trailing whitespace to be trimmed"),
    }
}

#[test]
fn swift_uses_two_space_indent() {
    // swift-format defaults to two-space indentation.
    let engine = TreeSitterEngine;
    let input = concat!("struct Point {\n", "let x: Int\n", "let y: Int\n", "}\n");
    let expected = concat!("struct Point {\n", "  let x: Int\n", "  let y: Int\n", "}\n");
    let s = src("test.swift", Language::Other("swift".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Swift must use 2-space indent");
}

#[test]
fn swift_switch_case_labels_align_with_switch_keyword() {
    // swift-format aligns case labels with `switch`, not inside the body.
    // Case label at depth 1 (same as `switch`), body at depth 2.
    let engine = TreeSitterEngine;
    let input = concat!(
        "func f() -> Int {\n",
        "switch shape {\n",
        "case .circle:\n",
        "return 1\n",
        "case .rect:\n",
        "return 2\n",
        "}\n",
        "}\n",
    );
    let expected = concat!(
        "func f() -> Int {\n",
        "  switch shape {\n",
        "  case .circle:\n",
        "    return 1\n",
        "  case .rect:\n",
        "    return 2\n",
        "  }\n",
        "}\n",
    );
    let s = src("test.swift", Language::Other("swift".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Swift case labels align with switch keyword");
}

#[test]
fn dart_switch_case_body_extra_indent() {
    // dart format indents case bodies one extra level past the case label.
    let engine = TreeSitterEngine;
    let input = concat!(
        "int f(int n) {\n",
        "switch (n) {\n",
        "case 0:\n",
        "return 0;\n",
        "default:\n",
        "return -1;\n",
        "}\n",
        "}\n",
    );
    let expected = concat!(
        "int f(int n) {\n",
        "  switch (n) {\n",
        "    case 0:\n",
        "      return 0;\n",
        "    default:\n",
        "      return -1;\n",
        "  }\n",
        "}\n",
    );
    let s = src("test.dart", Language::Other("dart".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Dart case body gets extra indent level");
}

#[test]
fn dart_closure_argument_not_over_indented() {
    // With the level-keyed-by-open-line model, `list.map((n) {` opens
    // two parens and a brace on the same line — they coalesce to one new
    // depth level (+1). The closure body is therefore at depth+1 (NOT +3),
    // and `})` releases that single level on its closing line.
    let engine = TreeSitterEngine;
    let input = concat!(
        "void main() {\n",
        "final result = list.map((n) {\n",
        "return n * 2;\n",
        "}).toList();\n",
        "}\n",
    );
    let expected = concat!(
        "void main() {\n",
        "  final result = list.map((n) {\n",
        "    return n * 2;\n",
        "  }).toList();\n",
        "}\n",
    );
    let s = src("test.dart", Language::Other("dart".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Dart closure body must not be over-indented");
}

// ── CRLF byte-cursor fix ─────────────────────────────────────────────────

#[test]
fn crlf_brace_counting_does_not_drift() {
    // Before the fix, `line.len() + 1` drifted by 1 per line on CRLF,
    // causing delimiters to miss their line window. Fix: `raw.len() + 1`.
    let engine = TreeSitterEngine;
    let crlf = "package main\r\n\r\nfunc main() {\r\nx := 1\r\n}\r\n";
    let lf = "package main\n\nfunc main() {\nx := 1\n}\n";
    let expected = "package main\n\nfunc main() {\n\tx := 1\n}\n";

    let crlf_src = src("main.go", Language::Other("go".into()), crlf);
    let lf_src = src("main.go", Language::Other("go".into()), lf);

    let crlf_out = formatted_text(engine.format(&crlf_src, &cfg(4)).unwrap(), crlf);
    let lf_out = formatted_text(engine.format(&lf_src, &cfg(4)).unwrap(), lf);

    assert_eq!(lf_out, expected, "LF Go reindented with tabs");
    assert_eq!(crlf_out, expected, "CRLF Go reindented identically (no byte drift)");
}

// ── paren/bracket continuation indent ────────────────────────────────────
// Go/Rust expected outputs verified by running gofmt/rustfmt on the inputs.

#[test]
fn go_multiline_call_args_get_continuation_indent() {
    // Ground truth: gofmt. Args one tab deeper than the call site.
    let engine = TreeSitterEngine;
    let input = concat!(
        "package main\n",
        "\n",
        "func main() {\n",
        "result, err := pkg.LongFunc(\n",
        "arg1,\n",
        "arg2,\n",
        ")\n",
        "_ = result\n",
        "_ = err\n",
        "}\n",
    );
    let expected = concat!(
        "package main\n",
        "\n",
        "func main() {\n",
        "\tresult, err := pkg.LongFunc(\n",
        "\t\targ1,\n",
        "\t\targ2,\n",
        "\t)\n",
        "\t_ = result\n",
        "\t_ = err\n",
        "}\n",
    );
    let s = src("main.go", Language::Other("go".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Go multi-line call args at +1 continuation depth");
}

#[test]
fn rust_multiline_call_args_get_continuation_indent() {
    // Ground truth: rustfmt. Names long enough to stay multi-line at 100-col.
    let engine = TreeSitterEngine;
    let input = concat!(
        "fn main() {\n",
        "let result = some_very_long_function_name(\n",
        "very_long_argument_one,\n",
        "very_long_argument_two,\n",
        "very_long_argument_three,\n",
        ");\n",
        "}\n",
    );
    let expected = concat!(
        "fn main() {\n",
        "    let result = some_very_long_function_name(\n",
        "        very_long_argument_one,\n",
        "        very_long_argument_two,\n",
        "        very_long_argument_three,\n",
        "    );\n",
        "}\n",
    );
    let s = src("main.rs", Language::Other("rust".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Rust multi-line call args at +1 continuation depth");
}

#[test]
fn java_multiline_call_args_get_continuation_indent() {
    // Expected output from the tier-2 generic reindenter (4-space): each
    // argument is one level deeper than the method body, `)` dedents back.
    let engine = TreeSitterEngine;
    let input = concat!(
        "class Foo {\n",
        "void method() {\n",
        "String result = SomeClass.longMethodName(\n",
        "arg1,\n",
        "arg2,\n",
        "arg3\n",
        ");\n",
        "}\n",
        "}\n",
    );
    let expected = concat!(
        "class Foo {\n",
        "    void method() {\n",
        "        String result = SomeClass.longMethodName(\n",
        "            arg1,\n",
        "            arg2,\n",
        "            arg3\n",
        "        );\n",
        "    }\n",
        "}\n",
    );
    let s = src("Test.java", Language::Other("java".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Java multi-line call args at +1 continuation depth");
}

#[test]
fn kotlin_multiline_call_args_get_continuation_indent() {
    // Expected output from the tier-2 generic reindenter (4-space): same
    // level-keyed-by-open-line behaviour as Java/Go/Rust.
    let engine = TreeSitterEngine;
    let input = concat!(
        "fun main() {\n",
        "val result = someObject.doTheThing(\n",
        "argument1,\n",
        "argument2,\n",
        ")\n",
        "println(result)\n",
        "}\n",
    );
    let expected = concat!(
        "fun main() {\n",
        "    val result = someObject.doTheThing(\n",
        "        argument1,\n",
        "        argument2,\n",
        "    )\n",
        "    println(result)\n",
        "}\n",
    );
    let s = src("main.kt", Language::Other("kotlin".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Kotlin multi-line call args at +1 continuation depth");
}

// ── regression: level-keyed-by-open-line fixes ───────────────────────────
// Failed under brace-line-dominance; must pass under the new model.

#[test]
fn java_constructor_paren_then_brace_close() {
    // Algorithm-expected (no Java formatter available as ground truth).
    // The `) {` pattern: `)` closes the constructor parameter list while `{`
    // opens the body on the same line. The body must be at class-depth+1 (=2),
    // not class-depth+2 (=3) as the old brace-line-dominance model produced.
    let engine = TreeSitterEngine;
    let input = concat!(
        "class Foo {\n",
        "Foo(\n",
        "Type arg\n",
        ") {\n",
        "this.arg = arg;\n",
        "}\n",
        "}\n",
    );
    let expected = concat!(
        "class Foo {\n",
        "    Foo(\n",
        "        Type arg\n",
        "    ) {\n",
        "        this.arg = arg;\n",
        "    }\n",
        "}\n",
    );
    let s = src("Foo.java", Language::Other("java".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, expected,
        "Java constructor body must be at class+1 depth, not class+2"
    );
}

#[test]
fn go_struct_in_call_close_then_paren_close_no_drift() {
    // Ground truth: `gofmt` on the same input produces the expected output.
    // `doThing(Config{` opens two brackets on one line — they coalesce to one
    // level. After `},` closes the struct, the `(` from `doThing(` is still
    // open at depth 1. The `)` then closes it; code after the call (`x := 1`)
    // must remain at depth 1, not drift to 0.
    let engine = TreeSitterEngine;
    let input = concat!(
        "package main\n",
        "\n",
        "func main() {\n",
        "doThing(Config{\n",
        "field: 1,\n",
        "},\n",
        ")\n",
        "x := 1\n",
        "}\n",
    );
    let expected = concat!(
        "package main\n",
        "\n",
        "func main() {\n",
        "\tdoThing(Config{\n",
        "\t\tfield: 1,\n",
        "\t},\n",
        "\t)\n",
        "\tx := 1\n",
        "}\n",
    );
    let s = src("main.go", Language::Other("go".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "code after struct-in-call must not drift to depth 0");
}

#[test]
fn double_brace_close_releases_two_levels() {
    // Algorithm-expected: `}}` on one line closes two levels opened on two
    // distinct lines, so both are released as leading closers before the
    // render depth is computed, giving depth 0 for the `}}` line itself.
    let engine = TreeSitterEngine;
    let input = concat!("class A {\n", "void f() {\n", "x = 1;\n", "}}\n",);
    let expected = concat!("class A {\n", "    void f() {\n", "        x = 1;\n", "}}\n",);
    let s = src("A.java", Language::Other("java".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "}}: two leading closers each release one level");
}

// ── LEAVE_UNTOUCHED: data / template / asset grammars ───────────────────────

#[test]
fn csv_with_trailing_whitespace_is_byte_identical_after_format() {
    // CSV fields may contain trailing spaces that are part of the value.
    // tier-2 must not strip them.
    let engine = TreeSitterEngine;
    // Note: no final newline — intentional to verify that policy is also not applied.
    let input = "id,name,value   \n1,foo ,42\n2,bar,  99   ";
    let s = src("data.csv", Language::Other("csv".into()), input);
    let out = engine.format(&s, &cfg(4)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "CSV must be returned Unchanged, got Formatted"
    );
}

#[test]
fn csv_emits_zero_lint_diagnostics() {
    // Even with trailing whitespace on every line, a CSV file must produce no
    // diagnostics — any change would silently corrupt field values.
    let engine = TreeSitterEngine;
    let input = "id,name   \n1,foo bar   \n2,baz   ";
    let s = src("data.csv", Language::Other("csv".into()), input);
    let diags = engine.lint(&s, &cfg(4)).unwrap();
    assert!(diags.is_empty(), "CSV must emit zero diagnostics, got {:?}", diags);
}

#[test]
fn erb_template_with_trailing_whitespace_is_byte_identical_after_format() {
    // Whitespace around ERB tags is rendered verbatim into the template output;
    // stripping it would change the HTML/text the template produces.
    let engine = TreeSitterEngine;
    // Trailing spaces on the first line are intentional template whitespace;
    // no final newline to also verify that policy is suppressed.
    let input = "<html>   \n<% items.each do |item| %>   \n  <%= item.name %>\n<% end %>";
    let s = src("page.erb", Language::Other("embeddedtemplate".into()), input);
    let out = engine.format(&s, &cfg(4)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "ERB must be returned Unchanged, got Formatted"
    );
}

#[test]
fn erb_emits_zero_lint_diagnostics() {
    // Same rationale as CSV: trailing whitespace in ERB is semantic output.
    let engine = TreeSitterEngine;
    let input = "<div>   \n  <%= value %>   \n</div>   ";
    let s = src("partial.erb", Language::Other("embeddedtemplate".into()), input);
    let diags = engine.lint(&s, &cfg(4)).unwrap();
    assert!(diags.is_empty(), "ERB must emit zero diagnostics, got {:?}", diags);
}

// ── Query-driven indent path ─────────────────────────────────────────────────
// These tests exercise the new query-driven reindent path for non-BRACE_FAMILY
// languages that have a bundled indents.scm in tree-sitter-language-pack 1.12.
// The test inputs are intentionally flat (all code at column 0); the expected
// outputs show structural reindentation at the correct depth with zero system
// tools — the only requirement is the grammar being available (downloaded on
// demand, exactly like the BRACE_FAMILY tests above).

/// Known-unformatted RON (Rusty Object Notation) fixture.
///
/// RON's indents.scm tags `(array)`, `(map)`, `(tuple)`, and `(struct)` with
/// `@indent`, plus `"{"/"}"`, `"("/")"`, `"["/ "]"` with `@branch`.  The
/// expected output applies 4-space indentation to the struct/tuple bodies.
#[test]
fn ron_query_driven_structural_reindent() {
    let engine = TreeSitterEngine;
    // Flat RON — every field at column 0, no indentation at all.
    let input = concat!(
        "Scene(\n",
        "name: \"test\",\n",
        "entities: [\n",
        "Entity(\n",
        "id: 1,\n",
        "),\n",
        "],\n",
        ")\n",
    );
    // After query-driven reindent: fields at +1 relative to enclosing
    // tuple/array, nested entities at +2.
    let expected = concat!(
        "Scene(\n",
        "    name: \"test\",\n",
        "    entities: [\n",
        "        Entity(\n",
        "            id: 1,\n",
        "        ),\n",
        "    ],\n",
        ")\n",
    );
    let s = src("scene.ron", Language::Other("ron".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "RON query-driven indent must nest correctly");
}

/// The query-driven path must protect the interior of a multi-line comment
/// exactly as the brace path does: leading whitespace inside a block comment is
/// author-formatted content, so it must survive byte-for-byte while the
/// surrounding code still reindents by structural depth. Without the
/// protected-range guard, the reindenter would trim and re-space the interior
/// lines, silently rewriting the comment body.
#[test]
fn ron_query_driven_reindent_preserves_multiline_comment_interior() {
    let engine = TreeSitterEngine;
    // Flat RON whose struct body opens with a block comment carrying
    // deliberately uneven interior indentation. Those interior lines — and the
    // closing `*/` line, whose leading whitespace is also comment content —
    // must be emitted verbatim; only the code lines reindent to depth 1.
    let input = concat!(
        "Scene(\n",
        "/* header\n",
        "        deeply indented note\n",
        "   shallow note\n",
        "*/\n",
        "name: \"x\",\n",
        ")\n",
    );
    let expected = concat!(
        "Scene(\n",
        "    /* header\n",
        "        deeply indented note\n",
        "   shallow note\n",
        "*/\n",
        "    name: \"x\",\n",
        ")\n",
    );
    let s = src("scene.ron", Language::Other("ron".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, expected,
        "comment interior must be verbatim while surrounding code reindents"
    );
    // The exact interior bytes between the comment delimiters survive.
    let interior = "\n        deeply indented note\n   shallow note\n";
    assert!(
        text.contains(interior),
        "comment interior must be preserved byte-for-byte"
    );
}

/// Regression guard: query path must not change already-correct RON.
#[test]
fn ron_query_driven_unchanged_when_already_indented() {
    let engine = TreeSitterEngine;
    let already_correct = concat!(
        "Scene(\n",
        "    name: \"test\",\n",
        "    entities: [\n",
        "        Entity(\n",
        "            id: 1,\n",
        "        ),\n",
        "    ],\n",
        ")\n",
    );
    let s = src("scene.ron", Language::Other("ron".into()), already_correct);
    let out = engine.format(&s, &cfg(4)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "already-indented RON must be Unchanged"
    );
}

// ── Elixir: built-in do/end indentation ─────────────────────────────────────
// Elixir uses `do...end` blocks rather than braces, so BRACE_FAMILY cannot
// reindent it. The built-in polylint indents query drives reindentation via the
// same query-driven path as RON/KDL, but with a query compiled from the static
// ELIXIR_INDENTS constant rather than a bundled indents.scm from the language pack.

/// Known-unformatted Elixir: the sample from the bug report — all content at
/// column 0 instead of the canonical 2-space nesting.
#[test]
fn elixir_do_end_reindents_nested_modules_and_defs() {
    let engine = TreeSitterEngine;
    let input = concat!("defmodule Foo do\n", "def bar do\n", ":ok\n", "end\n", "end\n",);
    let expected = concat!("defmodule Foo do\n", "  def bar do\n", "    :ok\n", "  end\n", "end\n",);
    let s = src("foo.ex", Language::Other("elixir".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Elixir do/end blocks must reindent to 2-space nesting");
}

/// Idempotency: already-correct Elixir must be returned as `Unchanged`.
#[test]
fn elixir_do_end_unchanged_when_already_indented() {
    let engine = TreeSitterEngine;
    let already_correct = concat!("defmodule Foo do\n", "  def bar do\n", "    :ok\n", "  end\n", "end\n",);
    let s = src("foo.ex", Language::Other("elixir".into()), already_correct);
    let out = engine.format(&s, &cfg(4)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "already-indented Elixir must be Unchanged"
    );
}

/// rescue/else/catch/after sub-blocks must sit at the same depth as `do`.
#[test]
fn elixir_rescue_block_at_same_depth_as_do() {
    let engine = TreeSitterEngine;
    let input = concat!("try do\n", "raise \"error\"\n", "rescue\n", "_ -> :ok\n", "end\n",);
    let expected = concat!("try do\n", "  raise \"error\"\n", "rescue\n", "  _ -> :ok\n", "end\n",);
    let s = src("foo.ex", Language::Other("elixir".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "rescue must be at same depth as do and end");
}

/// Anonymous functions (`fn ... end`) must indent their body by one level.
#[test]
fn elixir_anonymous_function_body_indented() {
    let engine = TreeSitterEngine;
    let input = concat!("add = fn x, y ->\n", "x + y\n", "end\n",);
    let expected = concat!("add = fn x, y ->\n", "  x + y\n", "end\n",);
    let s = src("foo.ex", Language::Other("elixir".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "fn ... end body must be indented");
}

#[test]
fn non_member_grammar_still_gets_whitespace_normalization() {
    // Regression guard: a language NOT in LEAVE_UNTOUCHED (bash) must still
    // receive trailing-whitespace stripping via normalize_whitespace.
    let engine = TreeSitterEngine;
    let input = "#!/bin/bash   \necho hello   \n";
    let s = src("script.sh", Language::Other("bash".into()), input);
    let out = engine.format(&s, &cfg(4)).unwrap();
    match out {
        FormatOutput::Formatted(text) => {
            assert_eq!(
                text, "#!/bin/bash\necho hello\n",
                "bash trailing whitespace must be stripped"
            );
        }
        FormatOutput::Unchanged => {
            panic!("bash with trailing whitespace must be Formatted (whitespace stripped), not Unchanged")
        }
    }
}
