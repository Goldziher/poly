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
fn kotlin_elvis_method_chain_preserves_continuation_indent() {
    let engine = TreeSitterEngine;
    let input = concat!(
        "fun configure() {\n",
        "    val manifest = configured?.let(::file) ?: generateSequence(projectDir) { it.parentFile }\n",
        "        .map { it.resolve(\"Cargo.toml\") }\n",
        "        .firstOrNull { it.isFile }\n",
        "        ?: throw GradleException(\n",
        "            \"Cannot locate manifest; \" +\n",
        "                \"set the manifest path explicitly\"\n",
        "        )\n",
        "}\n",
    );
    let source = src("build.gradle.kts", Language::Other("kotlin".into()), input);
    let text = formatted_text(engine.format(&source, &cfg(4)).unwrap(), input);
    assert_eq!(text, input, "Kotlin Elvis method-chain continuation indentation");
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

/// A grammar with neither a bundled nor a built-in indents query still reaches
/// whitespace normalization: trailing whitespace is stripped and nothing is
/// reindented. This is the guarantee `non_member_grammar_still_gets_whitespace_
/// normalization` used to encode with bash as its example; bash is now a
/// `BUILTIN_QUERIES` member, so the guarantee is restated here against a grammar
/// the pack cannot resolve at all.
#[test]
fn non_member_grammar_still_gets_whitespace_normalization() {
    let engine = TreeSitterEngine;
    let input = "alpha   \n    beta\t\n";
    let s = src("notes.unknownext", Language::Other("no-such-grammar".into()), input);
    let out = engine.format(&s, &cfg(4)).unwrap();
    match out {
        FormatOutput::Formatted(text) => {
            assert_eq!(
                text, "alpha\n    beta\n",
                "trailing whitespace must be stripped and leading indentation left alone"
            );
        }
        FormatOutput::Unchanged => {
            panic!("trailing whitespace must be Formatted (whitespace stripped), not Unchanged")
        }
    }
}

/// bash now has a built-in indents query, but a flat script contains no block
/// construct for it to capture. `try_reindent_builtin` returns `None` on an
/// empty capture set, so the file still lands on whitespace normalization
/// rather than being flattened to column 0.
#[test]
fn bash_script_without_blocks_still_gets_whitespace_normalization() {
    let engine = TreeSitterEngine;
    let input = "#!/bin/bash   \necho hello   \n";
    let s = src("script.sh", Language::Other("bash".into()), input);
    let out = engine.format(&s, &cfg(2)).unwrap();
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

/// Reindent `input` as bash at two-space width and return the resulting text.
fn bash(input: &str) -> String {
    let engine = TreeSitterEngine;
    let s = src("script.sh", Language::Other("bash".into()), input);
    formatted_text(engine.format(&s, &cfg(2)).unwrap(), input)
}

#[test]
fn bash_if_then_elif_else_fi_reindents_each_branch_body() {
    let input = concat!(
        "if [ -n \"$1\" ]; then\n",
        "echo yes\n",
        "elif test -f x; then\n",
        "echo maybe\n",
        "else\n",
        "echo no\n",
        "fi\n",
    );
    let expected = concat!(
        "if [ -n \"$1\" ]; then\n",
        "  echo yes\n",
        "elif test -f x; then\n",
        "  echo maybe\n",
        "else\n",
        "  echo no\n",
        "fi\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "if/elif/else bodies indent one level; the branch keywords and `fi` stay at the `if` level"
    );
}

#[test]
fn bash_while_and_for_do_done_reindent_bodies() {
    let input = concat!(
        "while read -r line; do\n",
        "echo \"$line\"\n",
        "done\n",
        "\n",
        "for i in 1 2 3; do\n",
        "echo \"$i\"\n",
        "done\n",
        "\n",
        "until false; do\n",
        "echo loop\n",
        "done\n",
    );
    let expected = concat!(
        "while read -r line; do\n",
        "  echo \"$line\"\n",
        "done\n",
        "\n",
        "for i in 1 2 3; do\n",
        "  echo \"$i\"\n",
        "done\n",
        "\n",
        "until false; do\n",
        "  echo loop\n",
        "done\n",
    );
    assert_eq!(bash(input), expected, "do...done bodies indent one level");
}

/// `case` patterns sit at the `case` level and their bodies — including the
/// `;;` terminator — one level in, matching `shfmt`'s default (switch-case
/// indentation off).
#[test]
fn bash_case_item_bodies_and_terminators_indent_one_level() {
    let input = concat!(
        "case \"$1\" in\n",
        "a)\n",
        "echo a\n",
        ";;\n",
        "b|c)\n",
        "echo bc\n",
        ";;\n",
        "*)\n",
        "echo other\n",
        ";;\n",
        "esac\n",
    );
    let expected = concat!(
        "case \"$1\" in\n",
        "a)\n",
        "  echo a\n",
        "  ;;\n",
        "b|c)\n",
        "  echo bc\n",
        "  ;;\n",
        "*)\n",
        "  echo other\n",
        "  ;;\n",
        "esac\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "case patterns stay at the `case` level; bodies and `;;` indent one level"
    );
}

#[test]
fn bash_function_body_and_brace_group_reindent() {
    let input = concat!(
        "myfunc() {\n",
        "local x=1\n",
        "echo \"$x\"\n",
        "}\n",
        "\n",
        "function other {\n",
        "echo hi\n",
        "}\n",
        "\n",
        "{\n",
        "echo group\n",
        "}\n",
    );
    let expected = concat!(
        "myfunc() {\n",
        "  local x=1\n",
        "  echo \"$x\"\n",
        "}\n",
        "\n",
        "function other {\n",
        "  echo hi\n",
        "}\n",
        "\n",
        "{\n",
        "  echo group\n",
        "}\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "compound-statement bodies indent one level and the closing brace returns to the opening level"
    );
}

#[test]
fn bash_subshell_and_command_substitution_reindent() {
    let input = concat!("(\n", "echo sub\n", ")\n", "\n", "x=$(\n", "echo cmd\n", ")\n");
    let expected = concat!("(\n", "  echo sub\n", ")\n", "\n", "x=$(\n", "  echo cmd\n", ")\n",);
    assert_eq!(bash(input), expected, "subshell and $( ) interiors indent one level");
}

/// Process substitution — `done < <( … )` — is a bracket-delimited block like
/// a subshell, and leaving it out of the model does not merely fail to indent
/// it: the interior is *de*-indented to column 0.
#[test]
fn bash_process_substitution_interior_reindents() {
    let input = concat!(
        "while read -r line; do\n",
        "echo \"$line\"\n",
        "done < <(\n",
        "find . -type f\n",
        "find bin -type f\n",
        ")\n",
    );
    let expected = concat!(
        "while read -r line; do\n",
        "  echo \"$line\"\n",
        "done < <(\n",
        "  find . -type f\n",
        "  find bin -type f\n",
        ")\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "the process-substitution body indents one level and its `)` returns to the opening level"
    );
}

#[test]
fn bash_array_literal_elements_indent_one_level() {
    let input = concat!("arr=(\n", "one\n", "two\n", ")\n");
    let expected = concat!("arr=(\n", "  one\n", "  two\n", ")\n");
    assert_eq!(bash(input), expected, "array elements indent one level");
}

/// The single most dangerous construct: a heredoc body is literal stdin and its
/// terminator must stay exactly where the author put it (column 0 for `<<`).
/// Reindenting either corrupts the script, so the whole heredoc — body and
/// terminator — is emitted verbatim even when the `cat` line itself moves.
#[test]
fn bash_heredoc_body_and_terminator_are_emitted_verbatim() {
    let input = concat!(
        "f() {\n",
        "cat <<EOT\n",
        "  two leading spaces\n",
        "no leading spaces\n",
        "EOT\n",
        "echo done\n",
        "}\n",
    );
    let expected = concat!(
        "f() {\n",
        "  cat <<EOT\n",
        "  two leading spaces\n",
        "no leading spaces\n",
        "EOT\n",
        "  echo done\n",
        "}\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "heredoc body lines and the `EOT` terminator must be byte-identical; only the `cat` line reindents"
    );
}

#[test]
fn bash_dash_heredoc_body_and_terminator_are_emitted_verbatim() {
    let input = concat!("f() {\n", "cat <<-'EOD'\n", "\ttabbed body\n", "\tEOD\n", "}\n",);
    let expected = concat!("f() {\n", "  cat <<-'EOD'\n", "\ttabbed body\n", "\tEOD\n", "}\n",);
    assert_eq!(
        bash(input),
        expected,
        "a <<- heredoc keeps its tab-indented body and terminator byte-for-byte"
    );
}

/// A backslash continuation is not a block, and poly cannot tell an aligned
/// continuation from an indented one. Every line after the first of a
/// multi-line command is emitted verbatim so poly never fights an author's (or
/// `shfmt`'s) chosen continuation alignment.
#[test]
fn bash_line_continuation_lines_are_left_verbatim() {
    let input = concat!("f() {\n", "foo \\\n", "    --bar \\\n", "    --baz\n", "}\n");
    let expected = concat!("f() {\n", "  foo \\\n", "    --bar \\\n", "    --baz\n", "}\n");
    assert_eq!(
        bash(input),
        expected,
        "the command's first line reindents; its continuation lines are untouched"
    );
}

#[test]
fn bash_and_or_list_and_pipeline_continuations_are_left_verbatim() {
    let input = concat!("foo &&\n", "  bar &&\n", "  baz\n", "\n", "a |\n", "  b |\n", "  c\n",);
    assert_eq!(
        bash(input),
        input,
        "`&&`/`||` list and pipeline continuation lines keep their existing alignment"
    );
}

/// `then` written on its own line is a real shell style. It belongs at the
/// `if` level, not one level in, so it dedents — but only because it starts its
/// line.
#[test]
fn bash_then_on_its_own_line_stays_at_the_if_level() {
    let input = concat!("if [ x ]\n", "then\n", "echo t\n", "fi\n");
    let expected = concat!("if [ x ]\n", "then\n", "  echo t\n", "fi\n");
    assert_eq!(bash(input), expected, "a leading `then` sits at the `if` level");
}

/// A closer that is *not* the first token on its line must not dedent that
/// line: `if ...; then ...; fi` written inline inside a function body is one
/// statement at the function's body level, and the trailing `fi` is incidental.
#[test]
fn bash_inline_fi_does_not_dedent_the_enclosing_block() {
    let input = concat!("f() {\n", "if [ y ]; then echo inline; fi\n", "}\n");
    let expected = concat!("f() {\n", "  if [ y ]; then echo inline; fi\n", "}\n");
    assert_eq!(
        bash(input),
        expected,
        "an inline `fi` is not at line start and must not pull the line out of the function body"
    );
}

/// A block opening inside a continuation shape beats the `@indent.keep` that
/// shape carries. `cmd | while …; do` is a `pipeline` spanning every line of
/// the loop, and keeping all of them verbatim would leave the loop body at
/// whatever indentation it arrived with while the pipeline's own line moved —
/// a file half-reindented in two different units.
#[test]
fn bash_block_opened_inside_a_pipeline_still_reindents() {
    let input = concat!(
        "f() {\n",
        "\tgit rev-parse | while read -r d; do\n",
        "\t\techo \"$d\"\n",
        "\tdone\n",
        "}\n",
    );
    let expected = concat!(
        "f() {\n",
        "  git rev-parse | while read -r d; do\n",
        "    echo \"$d\"\n",
        "  done\n",
        "}\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "the do...done body inside a pipeline must reindent, not stay verbatim"
    );
}

/// Two blocks that open on the *same* line are one indent level, not two — the
/// same level-keyed-by-open-line rule the bracket path uses. Here a `case`
/// pattern and a `{ … }` group both open on the pattern line, and counting them
/// separately would indent the group's body two levels past a pattern that is
/// itself at column 0.
#[test]
fn bash_two_blocks_opening_on_one_line_are_one_level() {
    let input = concat!(
        "case \"$h\" in\n",
        "[0-9]*) [ \"${#h}\" -eq 64 ] || {\n",
        "echo bad >&2\n",
        "exit 1\n",
        "} ;;\n",
        "esac\n",
    );
    let expected = concat!(
        "case \"$h\" in\n",
        "[0-9]*) [ \"${#h}\" -eq 64 ] || {\n",
        "  echo bad >&2\n",
        "  exit 1\n",
        "} ;;\n",
        "esac\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "the case item and the brace group share an opening line, so they contribute one level"
    );
}

/// The complement: the continuation lines *before* a block opens are still
/// kept, so only the part of the range the block actually covers is surrendered.
#[test]
fn bash_continuation_before_an_inner_block_is_still_kept() {
    let input = concat!(
        "f() {\n",
        "foo --a \\\n",
        "    --b | while read -r d; do\n",
        "echo \"$d\"\n",
        "done\n",
        "}\n",
    );
    let expected = concat!(
        "f() {\n",
        "  foo --a \\\n",
        "    --b | while read -r d; do\n",
        "    echo \"$d\"\n",
        "  done\n",
        "}\n",
    );
    assert_eq!(
        bash(input),
        expected,
        "the backslash continuation keeps its alignment while the loop body reindents"
    );
}

#[test]
fn bash_nested_blocks_indent_cumulatively() {
    let input = concat!(
        "f() {\n",
        "for i in 1 2; do\n",
        "if [ \"$i\" = 1 ]; then\n",
        "echo one\n",
        "fi\n",
        "done\n",
        "}\n",
    );
    let expected = concat!(
        "f() {\n",
        "  for i in 1 2; do\n",
        "    if [ \"$i\" = 1 ]; then\n",
        "      echo one\n",
        "    fi\n",
        "  done\n",
        "}\n",
    );
    assert_eq!(bash(input), expected, "nested blocks accumulate one level each");
}

#[test]
fn bash_already_two_space_indented_script_is_unchanged() {
    let engine = TreeSitterEngine;
    let already_correct = concat!(
        "#!/usr/bin/env bash\n",
        "set -euo pipefail\n",
        "\n",
        "main() {\n",
        "  local target=$1\n",
        "  if [ -d \"$target\" ]; then\n",
        "    for f in \"$target\"/*; do\n",
        "      echo \"$f\"\n",
        "    done\n",
        "  else\n",
        "    cat <<EOT\n",
        "missing: $target\n",
        "EOT\n",
        "  fi\n",
        "  case \"$target\" in\n",
        "  a)\n",
        "    echo a\n",
        "    ;;\n",
        "  *)\n",
        "    echo other\n",
        "    ;;\n",
        "  esac\n",
        "}\n",
        "\n",
        "main \"$@\"\n",
    );
    let s = src("script.sh", Language::Other("bash".into()), already_correct);
    let out = engine.format(&s, &cfg(2)).unwrap();
    assert!(
        matches!(out, FormatOutput::Unchanged),
        "an already two-space-indented script must come out byte-identical, got {:?}",
        formatted_text(engine.format(&s, &cfg(2)).unwrap(), already_correct)
    );
}

#[test]
fn bash_reindent_is_idempotent() {
    let input = concat!(
        "f() {\n",
        "case $1 in\n",
        "a)\n",
        "cat <<EOT\n",
        "raw\n",
        "EOT\n",
        ";;\n",
        "esac\n",
        "}\n",
    );
    let once = bash(input);
    let twice = bash(&once);
    assert_eq!(once, twice, "a second reindent pass must be a no-op");
}

/// A file the bash grammar cannot parse yields a tree full of `ERROR` nodes
/// whose ranges do not describe the real structure — most dangerously, a
/// heredoc inside one is no longer recognized as protected. Fall back to
/// whitespace normalization instead of reindenting from a broken tree.
#[test]
fn bash_unparsable_source_falls_back_to_whitespace_normalization() {
    let input = "f() {\n  esac fi done )   \n}\n";
    assert_eq!(
        bash(input),
        "f() {\n  esac fi done )\n}\n",
        "a tree with errors must only get whitespace normalization, leaving indentation alone"
    );
}
