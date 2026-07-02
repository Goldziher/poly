//! Catalog integrity + model tests. These validate the **vendored data** parses
//! and is internally consistent, and that the model helpers behave — they do not
//! invoke any external tool (the golden command fixtures are exercised by the
//! native-tool engine, which is where execution lives).

use serde::Deserialize;

use crate::{Catalog, PATH_PLACEHOLDER};

/// One `(input, output)` command fixture from the vendored `data/golden.json`.
#[derive(Debug, Deserialize)]
struct GoldenFixture {
    tool: String,
    command: String,
    language: String,
    input: String,
    output: String,
}

const GOLDEN_JSON: &str = include_str!("../data/golden.json");

#[test]
fn catalog_parses_and_is_non_trivial() {
    let catalog = Catalog::get();
    // The vendored snapshot carries hundreds of tools; guard against an empty or
    // truncated embed without pinning the exact count (which drifts on refresh).
    assert!(
        catalog.tools().len() > 300,
        "expected the full vendored catalog, got {}",
        catalog.tools().len()
    );
}

#[test]
fn every_tool_has_a_binary_and_at_least_one_command() {
    for tool in Catalog::get().tools() {
        assert!(!tool.name.is_empty(), "tool with empty name");
        assert!(!tool.binary.is_empty(), "{} has empty binary", tool.name);
        assert!(!tool.commands.is_empty(), "{} has no commands", tool.name);
    }
}

#[test]
fn lookup_resolves_known_tools_and_misses_unknown() {
    let catalog = Catalog::get();
    assert!(catalog.tool("shfmt").is_some());
    assert!(catalog.tool("gofmt").is_some());
    assert!(catalog.tool("this-tool-does-not-exist").is_none());
}

#[test]
fn shfmt_is_a_path_based_formatter() {
    let shfmt = Catalog::get().tool("shfmt").expect("shfmt present");
    assert_eq!(shfmt.binary, "shfmt");
    assert!(shfmt.is_formatter());
    let (_name, command) = shfmt.format_command().expect("shfmt formats");
    assert!(command.uses_path(), "shfmt formats a file path");
    assert!(!command.stdin);
}

#[test]
fn a_stdin_tool_is_flagged_as_stdin() {
    // cedar's `format` command reads source on stdin rather than a path.
    let cedar = Catalog::get().tool("cedar").expect("cedar present");
    let command = cedar.command("format").expect("cedar has a format command");
    assert!(command.stdin);
    assert!(!command.uses_path());
}

#[test]
fn pure_linter_does_not_masquerade_as_formatter() {
    // shellcheck is a linter only; it must not surface a format command.
    if let Some(shellcheck) = Catalog::get().tool("shellcheck") {
        assert!(shellcheck.is_linter());
        assert!(shellcheck.format_command().is_none(), "shellcheck is not a formatter");
    }
}

#[test]
fn argv_substitutes_the_path_placeholder() {
    let gofmt = Catalog::get().tool("gofmt").expect("gofmt present");
    let (_name, command) = gofmt.format_command().expect("gofmt formats");
    let argv = command.argv("/work/main.go");
    assert!(
        argv.iter().any(|argument| argument == "/work/main.go"),
        "expected the path substituted into argv, got {argv:?}"
    );
    assert!(
        !argv.iter().any(|argument| argument == PATH_PLACEHOLDER),
        "no placeholder should survive substitution"
    );
}

#[test]
fn golden_fixtures_reference_real_catalog_commands() {
    let catalog = Catalog::get();
    let fixtures: Vec<GoldenFixture> = serde_json::from_str(GOLDEN_JSON).expect("vendored golden.json must be valid");
    assert!(!fixtures.is_empty(), "expected golden fixtures");

    for fixture in &fixtures {
        let tool = catalog
            .tool(&fixture.tool)
            .unwrap_or_else(|| panic!("golden fixture names unknown tool {}", fixture.tool));
        assert!(
            tool.command(&fixture.command).is_some(),
            "golden fixture for {} names unknown command {:?}",
            fixture.tool,
            fixture.command
        );
        assert!(!fixture.language.is_empty());
        // `input` / `output` may legitimately be empty (an empty file formats to
        // empty); we only assert the fixture is wired to a real catalog command.
        let _ = (&fixture.input, &fixture.output);
    }
}

#[test]
fn actionlint_has_path_globs_scoped_to_workflows() {
    let catalog = Catalog::get();
    let tool = catalog.tool("actionlint").expect("actionlint present");
    assert!(
        !tool.path_globs.is_empty(),
        "actionlint must declare path_globs (it only lints GitHub Actions workflows)"
    );
    assert!(
        tool.path_globs.iter().any(|g| g.contains(".github/workflows")),
        "actionlint path_globs must reference .github/workflows; got: {:?}",
        tool.path_globs
    );
}
