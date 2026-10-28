//! Syntax that upstream rejected or mis-parsed: heredocs, multi-name `ARG`, empty values, continuations interleaved
//! with comments and blank lines, a byte order mark, and the backtick `escape` directive.

use dockerfile_parser::*;
use pretty_assertions::assert_eq;

fn parse(input: &str) -> Dockerfile {
    Dockerfile::parse(input).unwrap_or_else(|error| panic!("{error}\n--- input ---\n{input}"))
}

fn text<'a>(dockerfile: &'a Dockerfile, span: &Span) -> &'a str {
    &dockerfile.content[span.start..span.end]
}

#[test]
fn run_heredoc_is_one_instruction_with_its_body() {
    let input = "FROM alpine\nRUN <<EOF\nset -e\napk add curl\nEOF\nCMD [\"sh\"]\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 3);
    assert!(dockerfile.instructions[1].as_run().is_some());
    assert!(dockerfile.instructions[2].as_cmd().is_some());

    let [heredoc] = &dockerfile.heredocs[..] else {
        panic!("expected exactly one heredoc, got {:?}", dockerfile.heredocs);
    };
    assert_eq!(heredoc.instruction, 1);
    assert_eq!(heredoc.delimiter, "EOF");
    assert!(!heredoc.strip_tabs && !heredoc.quoted);
    assert_eq!(heredoc.body.content, "set -e\napk add curl\n");
    assert_eq!(text(&dockerfile, &heredoc.body.span), "set -e\napk add curl\n");
    assert_eq!(text(&dockerfile, &heredoc.span), "set -e\napk add curl\nEOF");
}

#[test]
fn heredoc_body_that_looks_like_instructions_is_not_parsed() {
    let input = "FROM alpine\nRUN <<'SETUP'\nFROM nothing\nARG BROKEN=\"\nesac\nSETUP\nUSER app\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 3);
    assert!(dockerfile.heredocs[0].quoted);
    assert_eq!(dockerfile.heredocs[0].delimiter, "SETUP");
    assert_eq!(
        dockerfile.instructions[2]
            .as_misc()
            .map(|m| m.instruction.content.as_str()),
        Some("USER")
    );
}

#[test]
fn heredoc_variants() {
    let input = "FROM alpine\nCOPY <<-EOT /etc/motd\n\tHello\n\tEOT\nRUN cat <<A <<\"B\"\none\nA\ntwo\nB\nRUN echo <<<here-string\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 4);
    let delimiters: Vec<_> = dockerfile
        .heredocs
        .iter()
        .map(|h| (h.instruction, h.delimiter.as_str(), h.strip_tabs, h.quoted))
        .collect();
    assert_eq!(
        delimiters,
        vec![(1, "EOT", true, false), (2, "A", false, false), (2, "B", false, true)]
    );
    assert_eq!(dockerfile.heredocs[1].body.content, "one\n");
}

#[test]
fn heredoc_inside_a_continued_run_line() {
    let dockerfile = parse("FROM alpine\nRUN python3 - \\\n  <<EOF\nprint(1)\nEOF\nENV A=1\n");

    assert_eq!(dockerfile.instructions.len(), 3);
    assert_eq!(dockerfile.heredocs[0].body.content, "print(1)\n");
}

#[test]
fn heredoc_marker_outside_run_copy_add_is_ignored() {
    let dockerfile = parse("FROM alpine\nLABEL a=\"<<EOF\"\nENV B=1\n");

    assert!(dockerfile.heredocs.is_empty());
    assert_eq!(dockerfile.instructions.len(), 3);
}

#[test]
fn unterminated_heredoc_swallows_the_rest_of_the_file_without_failing() {
    let dockerfile = parse("FROM alpine\nRUN <<EOF\necho hi\n");

    assert_eq!(dockerfile.instructions.len(), 2);
    assert_eq!(dockerfile.heredocs[0].body.content, "echo hi\n");
}

#[test]
fn arg_with_several_names_yields_one_instruction_each() {
    let dockerfile = parse("ARG A=1 B C=\"x y\"\nFROM alpine\n");

    let args: Vec<_> = dockerfile
        .global_args
        .iter()
        .map(|arg| {
            (
                arg.name.content.as_str(),
                arg.value.as_ref().map(|v| v.content.as_str()),
            )
        })
        .collect();
    assert_eq!(args, vec![("A", Some("1")), ("B", None), ("C", Some("x y"))]);
    assert_eq!(dockerfile.instructions.len(), 4);
    for arg in &dockerfile.global_args {
        assert_eq!(text(&dockerfile, &arg.span), "ARG A=1 B C=\"x y\"");
        assert_eq!(text(&dockerfile, &arg.name.span), arg.name.content);
    }
}

#[test]
fn empty_values_are_accepted() {
    let dockerfile = parse("FROM alpine\nARG A=\nENV B= C=2\n");

    let arg = dockerfile.instructions[1].as_arg().expect("arg");
    assert_eq!(arg.value.as_ref().map(|v| v.content.as_str()), Some(""));

    let Instruction::Env(env) = &dockerfile.instructions[2] else {
        panic!("expected ENV");
    };
    let pairs: Vec<_> = env.vars.iter().map(|var| var.key.content.as_str()).collect();
    assert_eq!(pairs, vec!["B", "C"]);
}

#[test]
fn continuation_skips_comments_and_blank_lines() {
    let input = "FROM alpine\nRUN echo a \\\n  # note\n\n  && echo b\nENV X=1\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 3);
    let run = dockerfile.instructions[1].as_run().expect("run");
    assert_eq!(text(&dockerfile, &run.span), "RUN echo a \\\n  # note\n\n  && echo b");
}

#[test]
fn continuation_ending_the_file_with_a_comment_parses() {
    let dockerfile = parse("FROM alpine\nRUN echo a \\\n# trailing comment\n");

    assert_eq!(dockerfile.instructions.len(), 2);
}

#[test]
fn shell_comment_after_run_does_not_swallow_the_next_instruction() {
    let dockerfile = parse("FROM alpine\nRUN # nothing\nENV A=1\n");

    assert_eq!(dockerfile.instructions.len(), 3);
}

#[test]
fn quoted_values_follow_docker_escaping() {
    let dockerfile = parse("FROM alpine\nENV A=\"cost: \\$5 \\\"q\\\" C:\\dir\" B='it\\'s'\nARG D=\"x\\\ny\"\n");

    let Instruction::Env(env) = &dockerfile.instructions[1] else {
        panic!("expected ENV");
    };
    let values: Vec<_> = env.vars.iter().map(|var| var.value.to_string()).collect();
    assert_eq!(values, vec!["cost: $5 \"q\" C:\\dir", "it's"]);
    assert_eq!(
        dockerfile.instructions[2]
            .as_arg()
            .expect("arg")
            .value
            .as_ref()
            .expect("value")
            .content,
        "xy"
    );
}

#[test]
fn exec_form_keeps_json_escapes() {
    let dockerfile = parse("FROM alpine\nCMD [\"echo\", \"a\\tb\"]\n");

    let cmd = dockerfile.instructions[1].as_cmd().expect("cmd");
    assert_eq!(cmd.expr.as_exec().expect("exec").as_str_vec(), vec!["echo", "a\tb"]);
}

#[test]
fn copy_accepts_exec_form() {
    let dockerfile = parse("FROM alpine\nCOPY --chown=1:1 [\"a b\", \"c\", \"/dst/\"]\n");

    let Instruction::Copy(copy) = &dockerfile.instructions[1] else {
        panic!("expected COPY");
    };
    let sources: Vec<_> = copy.sources.iter().map(|s| s.content.as_str()).collect();
    assert_eq!(sources, vec!["a b", "c"]);
    assert_eq!(copy.destination.content, "/dst/");
    assert_eq!(copy.flags.len(), 1);
}

#[test]
fn byte_order_mark_is_ignored_and_spans_stay_byte_exact() {
    let input = "\u{feff}FROM alpine AS base\nRUN true\n";
    let dockerfile = parse(input);

    let from = dockerfile.instructions[0].as_from().expect("from");
    assert_eq!(text(&dockerfile, &from.span), "FROM alpine AS base");
    assert_eq!(text(&dockerfile, &from.image.span), "alpine");
}

#[test]
fn backtick_escape_directive_continues_lines() {
    let input = "# escape=`\nFROM windows\nRUN dir `\n  C:\\tmp\nCOPY a C:\\dst\\sub\nRUN echo done C:\\\nENV A=1\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 5);
    let run = dockerfile.instructions[1].as_run().expect("run");
    assert_eq!(text(&dockerfile, &run.span), "RUN dir `\n  C:\\tmp");
    let Instruction::Copy(copy) = &dockerfile.instructions[2] else {
        panic!("expected COPY");
    };
    assert_eq!(text(&dockerfile, &copy.destination.span), "C:\\dst\\sub");
}

#[test]
fn backtick_escape_trailing_backslash_stays_inside_the_instruction_span() {
    let input = "# escape=`\nFROM windows\nENV A=C:\\\nRUN echo done C:\\\nCOPY a C:\\dst\\\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 4);
    let spans: Vec<&str> = dockerfile
        .instructions
        .iter()
        .map(|instruction| text(&dockerfile, &instruction.span()))
        .collect();
    assert_eq!(
        spans,
        ["FROM windows", "ENV A=C:\\", "RUN echo done C:\\", "COPY a C:\\dst\\"]
    );
}

#[test]
fn crlf_line_endings_with_heredoc() {
    let dockerfile = parse("FROM alpine\r\nRUN <<EOF\r\necho hi\r\nEOF\r\nENV A=1\r\n");

    assert_eq!(dockerfile.instructions.len(), 3);
    assert_eq!(dockerfile.heredocs[0].body.content, "echo hi\r\n");
}

#[test]
fn syntax_directive_and_mount_flags_parse() {
    let input = "# syntax=docker/dockerfile:1.7\nFROM --platform=$BUILDPLATFORM rust:1 AS build\nRUN --mount=type=cache,target=/root/.cargo --network=none cargo build\n";
    let dockerfile = parse(input);

    assert_eq!(dockerfile.instructions.len(), 2);
}
