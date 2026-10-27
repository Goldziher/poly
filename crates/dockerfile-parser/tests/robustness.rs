//! Property tests for the guarantees poly relies on when it runs the parser in-process: parsing never panics, every
//! reported span indexes the original input, and splicing an instruction's own text back changes nothing.

use dockerfile_parser::*;
use proptest::prelude::*;

const FRAGMENTS: &[&str] = &[
    "FROM ",
    "from ",
    "RUN ",
    "ARG ",
    "ENV ",
    "LABEL ",
    "COPY ",
    "ADD ",
    "CMD ",
    "ENTRYPOINT ",
    "ONBUILD ",
    "USER ",
    "alpine:3.19",
    "a",
    "B=c",
    "=",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    "\\\n",
    "\\",
    "`",
    "# ",
    "#",
    "# escape=`\n",
    "# syntax=docker/dockerfile:1\n",
    "\"",
    "'",
    "\\\"",
    "$",
    "${",
    "}",
    "${A}",
    "$A",
    "<<",
    "<<EOF\n",
    "<<-X\n",
    "<<'E'\n",
    "EOF\n",
    "EOF",
    "X\n",
    "\tX\n",
    "[",
    "]",
    ",",
    "[\"a\", \"b\"]",
    "--mount=type=cache,target=/x ",
    "--from=0 ",
    "--platform=linux/amd64 ",
    "@sha256:",
    "AS ",
    "as ",
    "\u{feff}",
    "é",
    "日本",
    "\u{0}",
    "\u{1f}",
];

fn arbitrary_dockerfile() -> impl Strategy<Value = String> {
    prop_oneof![
        proptest::collection::vec(proptest::sample::select(FRAGMENTS), 0..60).prop_map(|parts| parts.concat()),
        "\\PC{0,200}",
    ]
}

fn assert_span_valid(content: &str, span: &Span, what: &str) {
    assert!(
        span.start <= span.end
            && span.end <= content.len()
            && content.is_char_boundary(span.start)
            && content.is_char_boundary(span.end),
        "invalid {what} span {span:?} for input of {} bytes: {content:?}",
        content.len()
    );
}

fn assert_spans_valid(dockerfile: &Dockerfile) {
    let content = &dockerfile.content;
    let mut previous_end = 0;
    for instruction in &dockerfile.instructions {
        let span = instruction.span();
        assert_span_valid(content, &span, "instruction");
        assert!(
            span.start >= previous_end || matches!(instruction, Instruction::Arg(_)),
            "instructions overlap"
        );
        previous_end = span.end;

        match instruction {
            Instruction::From(from) => {
                assert_span_valid(content, &from.image.span, "from image");
                if let Some(alias) = &from.alias {
                    assert_span_valid(content, &alias.span, "from alias");
                }
            }
            Instruction::Arg(arg) => {
                assert_span_valid(content, &arg.name.span, "arg name");
                if let Some(value) = &arg.value {
                    assert_span_valid(content, &value.span, "arg value");
                }
            }
            Instruction::Copy(copy) => {
                for path in copy.sources.iter().chain([&copy.destination]) {
                    assert_span_valid(content, &path.span, "copy path");
                }
            }
            Instruction::Env(env) => {
                for var in &env.vars {
                    assert_span_valid(content, &var.span, "env var");
                    assert_span_valid(content, &var.key.span, "env key");
                }
            }
            Instruction::Label(label) => {
                for label in &label.labels {
                    assert_span_valid(content, &label.name.span, "label name");
                    assert_span_valid(content, &label.value.span, "label value");
                }
            }
            Instruction::Misc(misc) => assert_span_valid(content, &misc.instruction.span, "misc keyword"),
            Instruction::Run(_) | Instruction::Entrypoint(_) | Instruction::Cmd(_) => {}
        }
    }

    for heredoc in &dockerfile.heredocs {
        assert_span_valid(content, &heredoc.span, "heredoc");
        assert_span_valid(content, &heredoc.body.span, "heredoc body");
        assert!(
            heredoc.instruction < dockerfile.instructions.len(),
            "heredoc owner out of range"
        );
    }
}

fn exercise(input: &str) {
    let Ok(dockerfile) = Dockerfile::parse(input) else {
        return;
    };

    assert_spans_valid(&dockerfile);

    let mut splicer = dockerfile.splicer();
    for instruction in &dockerfile.instructions {
        let span = instruction.span();
        splicer.splice(&span, &input[span.start..span.end]);
        let _ = span.relative_span(&dockerfile);
    }
    assert_eq!(
        splicer.content, input,
        "splicing instructions with their own text must be a no-op"
    );

    for stage in dockerfile.iter_stages() {
        for instruction in stage.instructions {
            if let Instruction::From(from) = instruction {
                let _ = from.image_parsed.resolve_vars(&dockerfile);
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn parsing_never_panics_and_spans_index_the_input(input in arbitrary_dockerfile()) {
        exercise(&input);
    }

    #[test]
    fn image_refs_never_panic(reference in "\\PC{0,60}") {
        let image = ImageRef::parse(&reference);
        let _ = image.to_string();
    }
}

#[test]
fn deeply_continued_input_fails_cleanly_instead_of_overflowing_the_stack() {
    let input = format!("FROM a\nRUN {}x\n", "a \\\n".repeat(200_000));

    let outcome = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || Dockerfile::parse(&input).is_ok())
        .expect("spawn")
        .join();

    assert!(outcome.is_ok(), "the parser must not crash the thread");
}

#[test]
fn large_inputs_parse_in_reasonable_time() {
    let input = format!(
        "FROM a\n{}",
        "RUN echo hello world\nENV A=b C=d\nLABEL x=y\n".repeat(20_000)
    );

    let started = std::time::Instant::now();
    let dockerfile = Dockerfile::parse(&input).expect("valid");

    assert_eq!(dockerfile.instructions.len(), 60_001);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "parse took {:?}",
        started.elapsed()
    );
}
