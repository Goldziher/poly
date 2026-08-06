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
    let engine = TreeSitterEngine;
    let input = concat!("struct Point {\n", "let x: Int\n", "let y: Int\n", "}\n");
    let expected = concat!("struct Point {\n", "  let x: Int\n", "  let y: Int\n", "}\n");
    let s = src("test.swift", Language::Other("swift".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "Swift must use 2-space indent");
}

#[test]
fn swift_switch_case_labels_align_with_switch_keyword() {
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

#[test]
fn crlf_brace_counting_does_not_drift() {
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

#[test]
fn go_multiline_call_args_get_continuation_indent() {
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

/// `java` is deliberately excluded from `BRACE_FAMILY` (see its module doc):
/// `tree-sitter-language-pack`'s pre-built per-platform grammar binaries are
/// not guaranteed byte-identical across releases, so poly must not derive
/// indentation from the java CST at all — only whitespace normalization,
/// which has no grammar dependency and is deterministic across platforms.
/// This locks in that a badly-indented java file is left with its original
/// (bad) indentation rather than being bracket-reindented.
#[test]
fn java_source_is_only_whitespace_normalized_never_bracket_reindented() {
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
    let s = src("Test.java", Language::Other("java".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, input,
        "java must never be bracket-reindented; input already has no trailing whitespace or \
         line-ending issues, so whitespace normalization must return it byte-identical"
    );
}

/// `csharp` is deliberately excluded from `BRACE_FAMILY` for the same reason
/// (see the module doc on `BRACE_FAMILY`): its external scanner's use of libc
/// wide-ctype functions makes its CST platform-dependent, so poly must not
/// derive indentation from it.
#[test]
fn csharp_source_is_only_whitespace_normalized_never_bracket_reindented() {
    let engine = TreeSitterEngine;
    let input = concat!(
        "public class Foo {\n",
        "public void Method() {\n",
        "var result = SomeClass.LongMethodName(\n",
        "arg1,\n",
        "arg2\n",
        ");\n",
        "}\n",
        "}\n",
    );
    let s = src("Test.cs", Language::Other("csharp".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, input,
        "csharp must never be bracket-reindented; input already has no trailing whitespace or \
         line-ending issues, so whitespace normalization must return it byte-identical"
    );
}

#[test]
fn kotlin_multiline_call_args_get_continuation_indent() {
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

#[test]
fn go_multiline_signature_paren_then_brace_close() {
    let engine = TreeSitterEngine;
    let input = concat!("func Foo(\n", "arg int,\n", ") {\n", "x = arg\n", "}\n",);
    let expected = concat!("func Foo(\n", "\targ int,\n", ") {\n", "\tx = arg\n", "}\n",);
    let s = src("foo.go", Language::Other("go".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, expected,
        "closing paren-then-brace must drop back to the pre-paren depth, not leave a phantom extra level"
    );
}

#[test]
fn go_struct_in_call_close_then_paren_close_no_drift() {
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
    let engine = TreeSitterEngine;
    let input = concat!("void f() {\n", "if (1) {\n", "x = 1;\n", "}}\n",);
    let expected = concat!("void f() {\n", "    if (1) {\n", "        x = 1;\n", "}}\n",);
    let s = src("a.c", Language::Other("c".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(text, expected, "}}: two leading closers each release one level");
}

#[test]
fn csv_with_trailing_whitespace_is_byte_identical_after_format() {
    let engine = TreeSitterEngine;
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
    let engine = TreeSitterEngine;
    let input = "id,name   \n1,foo bar   \n2,baz   ";
    let s = src("data.csv", Language::Other("csv".into()), input);
    let diags = engine.lint(&s, &cfg(4)).unwrap();
    assert!(diags.is_empty(), "CSV must emit zero diagnostics, got {:?}", diags);
}

#[test]
fn erb_template_with_trailing_whitespace_is_byte_identical_after_format() {
    let engine = TreeSitterEngine;
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
    let engine = TreeSitterEngine;
    let input = "<div>   \n  <%= value %>   \n</div>   ";
    let s = src("partial.erb", Language::Other("embeddedtemplate".into()), input);
    let diags = engine.lint(&s, &cfg(4)).unwrap();
    assert!(diags.is_empty(), "ERB must emit zero diagnostics, got {:?}", diags);
}

/// Known-unformatted RON (Rusty Object Notation) fixture.
///
/// RON's indents.scm tags `(array)`, `(map)`, `(tuple)`, and `(struct)` with
/// `@indent`, plus `"{"/"}"`, `"("/")"`, `"["/ "]"` with `@branch`.  The
/// expected output applies 4-space indentation to the struct/tuple bodies.
#[test]
fn ron_query_driven_structural_reindent() {
    let engine = TreeSitterEngine;
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

/// A `mix format`-formatted map must survive untouched. poly previously trimmed
/// every line and re-emitted it at the computed level — 0 for a top-level map,
/// since the query modelled only `do`/`fn` blocks — so it flattened the map to
/// column 0 and then oscillated against `mix format` forever.
#[test]
fn elixir_mix_formatted_map_is_unchanged() {
    let engine = TreeSitterEngine;
    let already_correct = concat!(
        "%{\n",
        "  \"libfoo-nif-2.16-aarch64-apple-darwin.so.tar.gz\" =>\n",
        "    \"sha256:0f0def70ac8ee555e3a5f67ebac652764f30f4252a97430f1edfebb35b5de3be\",\n",
        "  \"libfoo-nif-2.16-x86_64-apple-darwin.so.tar.gz\" =>\n",
        "    \"sha256:cd5c2391a37d047e4ca40a70cd3ccb624ec1361fd957db09d5ef43059a37f611\"\n",
        "}\n",
    );
    let s = src("checksum.exs", Language::Other("elixir".into()), already_correct);
    let out = engine.format(&s, &cfg(4)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "a mix-formatted Elixir map must be left byte-for-byte, got {:?}",
        formatted_text(engine.format(&s, &cfg(4)).unwrap(), already_correct)
    );
}

/// The same map nested inside a `do` block: the block still indents, but the
/// map's interior lines keep their `mix format` alignment.
#[test]
fn elixir_map_inside_do_block_keeps_interior_alignment() {
    let engine = TreeSitterEngine;
    let input = concat!(
        "defmodule Foo do\n",
        "def checksums do\n",
        "%{\n",
        "  \"a\" => \"1\",\n",
        "  \"b\" => \"2\"\n",
        "}\n",
        "end\n",
        "end\n",
    );
    let expected = concat!(
        "defmodule Foo do\n",
        "  def checksums do\n",
        "    %{\n",
        "  \"a\" => \"1\",\n",
        "  \"b\" => \"2\"\n",
        "    }\n",
        "  end\n",
        "end\n",
    );
    let s = src("foo.ex", Language::Other("elixir".into()), input);
    let text = formatted_text(engine.format(&s, &cfg(4)).unwrap(), input);
    assert_eq!(
        text, expected,
        "the do-block reindents but the map interior is emitted verbatim"
    );
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
