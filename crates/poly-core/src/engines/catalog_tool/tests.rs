use std::collections::BTreeMap;
use std::path::PathBuf;

use poly_catalog::{Catalog, Command as CatalogCommand};

use super::*;
use crate::config::{EngineConfig, GlobalDefaults};

/// Build a leaked `&'static Tool` for a single-command catalog tool, so the
/// `&'static Tool` contract is satisfied without a real catalog entry.
fn leak_tool(name: &str, binary: &str, category: &str, arguments: Vec<String>) -> &'static Tool {
    Box::leak(Box::new(Tool {
        name: name.to_string(),
        binary: binary.to_string(),
        categories: vec![category.to_string()],
        languages: vec!["text".to_string()],
        commands: BTreeMap::from([(
            String::new(),
            CatalogCommand {
                arguments,
                stdin: false,
            },
        )]),
        homepage: String::new(),
        path_globs: vec![],
    }))
}

fn make_src(path: &str, content: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from(path),
        language: Language::Other("test".to_string()),
        content: content.into(),
    }
}

fn cfg() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options: toml::Table::new(),
    }
}

/// Convenience wrapper for tests: build an engine with empty env and no root.
fn format_engine_default(
    tool: &'static Tool,
    command_name: Option<&str>,
    args_override: Option<&[String]>,
) -> Option<CatalogToolEngine> {
    CatalogToolEngine::format_engine(tool, command_name, args_override, BTreeMap::new(), None)
}

/// Convenience wrapper for tests: build a lint engine with empty env and no root.
fn lint_engine_default(
    tool: &'static Tool,
    command_name: Option<&str>,
    args_override: Option<&[String]>,
) -> Option<CatalogToolEngine> {
    CatalogToolEngine::lint_engine(tool, command_name, args_override, BTreeMap::new(), None)
}

#[test]
fn format_engine_builds_for_a_catalog_formatter() {
    let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
    let engine = format_engine_default(tool, None, None).expect("shfmt exposes a format command");
    assert_eq!(engine.name(), "shfmt");
    assert!(engine.capabilities().format);
    assert!(!engine.capabilities().lint);
    assert!(engine.version().contains("shfmt"));
}

#[test]
fn format_engine_none_for_pure_linter() {
    if let Some(tool) = Catalog::get().tool("shellcheck") {
        assert!(format_engine_default(tool, None, None).is_none());
    }
}

#[test]
fn args_override_replaces_catalog_argv() {
    let tool = Catalog::get().tool("shfmt").expect("shfmt in catalog");
    let engine = format_engine_default(tool, None, Some(&["--custom".to_string()])).unwrap();
    assert_eq!(engine.arguments, vec!["--custom".to_string()]);
    assert!(engine.version().contains("--custom"));
}

#[test]
fn argv_substitutes_path_placeholder() {
    let tool = Catalog::get().tool("gofmt").expect("gofmt in catalog");
    let engine = format_engine_default(tool, None, None).unwrap();
    let argv = engine.argv_with_path("/tmp/x.go");
    assert!(argv.iter().any(|a| a == "/tmp/x.go"));
    assert!(!argv.iter().any(|a| a == PATH_PLACEHOLDER));
}

#[test]
fn lint_engine_rejects_a_mutating_command() {
    for flag in ["--fix", "--write", "-w", "-i"] {
        let tool = leak_tool(
            "fakefixer",
            "true",
            "linter",
            vec![flag.to_string(), PATH_PLACEHOLDER.to_string()],
        );
        assert!(
            lint_engine_default(tool, None, None).is_none(),
            "mutating flag `{flag}` must be rejected as a linter"
        );
    }
}

#[test]
fn lint_engine_rejects_a_mutating_subcommand() {
    for subcommand in ["fix", "format", "fmt", "write"] {
        let tool = leak_tool(
            "fakesubfixer",
            "true",
            "linter",
            vec![subcommand.to_string(), PATH_PLACEHOLDER.to_string()],
        );
        assert!(
            lint_engine_default(tool, None, None).is_none(),
            "mutating subcommand `{subcommand}` must be rejected as a linter"
        );
    }
}

/// An **independent** oracle for "this argv rewrites the file it is handed",
/// deliberately spelled out here instead of delegating to the production
/// [`is_mutating`]. A test that reused the production predicate would pass
/// whenever production and test shared the same blind spot — which is precisely
/// how `sqruff fix $PATH` shipped as a catalog lint command.
fn argv_rewrites_the_file(arguments: &[String]) -> bool {
    const REWRITES: &[&str] = &[
        "--auto",
        "--auto-correct",
        "--autocorrect",
        "--autofix",
        "--edit",
        "--fix",
        "--fix-layout",
        "--in-place",
        "--reformat",
        "--replace",
        "--write",
        "--write-changes",
        "-fix",
        "-i",
        "-w",
        "apply",
        "fix",
        "fmt",
        "format",
        "write",
    ];
    arguments.iter().any(|argument| REWRITES.contains(&argument.as_str()))
}

/// Nothing poly ships may wire as a catalog linter with an argv that rewrites
/// the file. `poly lint` promises to report, never to edit; a mutating lint argv
/// destroys the user's source *and* reports it clean, because a fix command
/// exits zero once it has finished fixing.
#[test]
fn no_shipped_catalog_tool_wires_a_mutating_lint_command() {
    let offenders: Vec<String> = Catalog::get()
        .tools()
        .iter()
        .filter_map(|tool| lint_engine_default(tool, None, None).map(|engine| (tool, engine)))
        .filter(|(_, engine)| argv_rewrites_the_file(&engine.arguments))
        .map(|(tool, engine)| format!("{} -> {:?}", tool.name, engine.arguments))
        .collect();
    assert!(
        offenders.is_empty(),
        "catalog tools wired as linters with a file-rewriting argv: {offenders:#?}"
    );
}

/// `pyupgrade` rewrites in place on every run and its catalog argv forces a
/// zero exit (`--exit-zero-even-if-changed`), so no argv inspection can make it
/// safe: it must be refused by name.
#[test]
fn lint_engine_rejects_always_mutating_tools() {
    for name in ALWAYS_MUTATING_TOOLS {
        let tool = Catalog::get().tool(name).unwrap_or_else(|| panic!("{name} in catalog"));
        assert!(
            lint_engine_default(tool, None, None).is_none(),
            "{name} rewrites files on every run and must not wire as a catalog linter"
        );
    }
}

/// The end-to-end guarantee, asserted on the **effect** rather than on any
/// particular mechanism: whatever a catalog tool's lint command looks like,
/// running [`Engine::lint`] must leave the file on disk byte-identical.
///
/// The stand-in binary is a real in-place fixer — it truncates whatever path it
/// is handed — wired with `sqruff`'s exact shipped argv shape (`<tool> fix
/// $PATH`). Before the mutating-subcommand gate existed this engine wired, ran
/// against the real on-disk file, and overwrote it.
#[cfg(unix)]
#[test]
fn lint_leaves_the_file_on_disk_byte_identical() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let fixer = dir.path().join("fakefixer");
    std::fs::write(
        &fixer,
        "#!/bin/sh\n[ \"$#\" -eq 2 ] && printf 'REWRITTEN\\n' > \"$2\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&fixer, std::fs::Permissions::from_mode(0o755)).unwrap();

    let file = dir.path().join("q.sql");
    let original = "select a,b from t where a=1;\n";
    std::fs::write(&file, original).unwrap();

    let tool = leak_tool(
        "fakefixer",
        fixer.to_str().unwrap(),
        "linter",
        vec!["fix".to_string(), PATH_PLACEHOLDER.to_string()],
    );

    // Declining the capability is the expected outcome, but the invariant under
    // test is the file's bytes — so a future fix that lints a throwaway copy
    // instead would keep this test honest rather than break it.
    if let Some(engine) = lint_engine_default(tool, None, None) {
        let _ = engine.lint(&make_src(file.to_str().unwrap(), original), &cfg());
    }

    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        original,
        "poly lint must never rewrite the source file it is linting"
    );
}

#[test]
fn lint_engine_rejects_a_mutating_args_override() {
    let tool = leak_tool("fakelint", "true", "linter", vec![PATH_PLACEHOLDER.to_string()]);
    assert!(lint_engine_default(tool, None, Some(&["--fix".to_string()])).is_none());
}

#[cfg(unix)]
#[test]
fn lint_engine_reports_one_diagnostic_on_nonzero_exit() {
    let tool = leak_tool(
        "fakelint",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            "echo 'problem on line 1' >&2\nexit 3".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
    );
    let engine = lint_engine_default(tool, None, None).expect("non-mutating linter wires");
    assert!(engine.capabilities().lint);
    assert!(!engine.capabilities().format);

    let diagnostics = engine.lint(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
    assert_eq!(diagnostics.len(), 1, "one file-level finding on failure");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.engine, "fakelint");
    assert_eq!(diagnostic.severity, Severity::Warning);
    assert!(diagnostic.span.is_none(), "no span at breadth-tier fidelity");
    assert!(diagnostic.code.is_none(), "no rule code");
    assert!(
        diagnostic.title.contains("problem on line 1"),
        "carries the tool's output: {}",
        diagnostic.title
    );
}

#[cfg(unix)]
#[test]
fn lint_engine_reports_nothing_on_zero_exit() {
    let tool = leak_tool(
        "oklint",
        "sh",
        "linter",
        vec!["-c".to_string(), "exit 0".to_string(), PATH_PLACEHOLDER.to_string()],
    );
    let engine = lint_engine_default(tool, None, None).unwrap();
    let diagnostics = engine.lint(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
    assert!(diagnostics.is_empty(), "a passing run yields no diagnostics");
}

#[cfg(unix)]
#[test]
fn lint_runs_against_real_file_when_content_matches_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("real.txt");
    std::fs::write(&file, "hello\n").unwrap();
    let tool = leak_tool(
        "pathecho",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            "printf '%s' \"$0\"\nexit 1".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
    );
    let engine = lint_engine_default(tool, None, None).unwrap();
    let src = make_src(file.to_string_lossy().as_ref(), "hello\n");
    let diags = engine.lint(&src, &cfg()).unwrap();
    assert_eq!(diags.len(), 1);
    assert!(
        !diags[0].title.contains("poly-catalog-"),
        "must run against the real file, not a temp copy: {}",
        diags[0].title
    );
    let canonical = std::fs::canonicalize(&file).unwrap();
    assert!(
        diags[0].title.contains(canonical.to_string_lossy().as_ref()),
        "diagnostic must carry the real path {}, got {}",
        canonical.display(),
        diags[0].title
    );
}

#[cfg(unix)]
#[test]
fn lint_falls_back_to_temp_copy_when_content_diverges() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("stale.txt");
    std::fs::write(&file, "on-disk\n").unwrap();
    let tool = leak_tool(
        "pathecho2",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            "printf '%s' \"$0\"\nexit 1".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
    );
    let engine = lint_engine_default(tool, None, None).unwrap();
    let src = make_src(file.to_string_lossy().as_ref(), "in-memory-different\n");
    let diags = engine.lint(&src, &cfg()).unwrap();
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].title.contains("poly-catalog-"),
        "diverging content must fall back to a temp copy: {}",
        diags[0].title
    );
}

#[test]
fn lint_engine_rejects_whole_project_type_checkers() {
    for name in ["pyrefly", "mypy", "ty"] {
        assert!(is_whole_project_linter(name), "{name} must be denylisted");
        if let Some(tool) = Catalog::get().tool(name) {
            assert!(
                lint_engine_default(tool, None, None).is_none(),
                "{name} is a whole-project type-checker and must not wire as a catalog linter"
            );
        }
    }
}

#[test]
fn lint_engine_allows_per_file_linters() {
    assert!(!is_whole_project_linter("shellcheck"));
}

#[test]
fn absent_binary_is_a_noop() {
    let tool = Catalog::get()
        .tools()
        .iter()
        .find(|t| t.format_command().is_some() && probe_binary(&t.binary).is_none());
    if let Some(tool) = tool {
        let engine = format_engine_default(tool, None, None).unwrap();
        let result = engine.format(&make_src("file.txt", "anything\n"), &cfg()).unwrap();
        assert!(matches!(result, FormatOutput::Unchanged));
    }
}

#[cfg(unix)]
#[test]
fn env_var_is_visible_to_the_spawned_process() {
    let tool = leak_tool(
        "envcheck",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            "printf '%s' \"$POLY_TEST_VAR\"\nexit 1".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
    );
    let env = BTreeMap::from([("POLY_TEST_VAR".to_string(), "hello-from-env".to_string())]);
    let engine = CatalogToolEngine::lint_engine(tool, None, None, env, None).expect("non-mutating linter wires");
    let diagnostics = engine.lint(&make_src("file.txt", "content\n"), &cfg()).unwrap();
    assert_eq!(diagnostics.len(), 1, "non-zero exit → one diagnostic");
    assert!(
        diagnostics[0].title.contains("hello-from-env"),
        "env var reflected in tool output: {}",
        diagnostics[0].title
    );
}

#[cfg(unix)]
#[test]
fn root_sets_the_working_directory_of_the_spawned_process() {
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    let tool = leak_tool(
        "cwdcheck",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            "pwd -P\nexit 1".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
    );
    let engine = CatalogToolEngine::lint_engine(tool, None, None, BTreeMap::new(), Some(tmp.clone()))
        .expect("non-mutating linter wires");
    let diagnostics = engine.lint(&make_src("file.txt", "content\n"), &cfg()).unwrap();
    assert_eq!(diagnostics.len(), 1, "non-zero exit → one diagnostic");
    let tmp_str = tmp.to_string_lossy();
    assert!(
        diagnostics[0].title.contains(tmp_str.as_ref()),
        "cwd reflects root override: {}",
        diagnostics[0].title
    );
}

/// Build a leaked `&'static Tool` with path_globs, for testing the path filter.
#[cfg(unix)]
fn leak_tool_with_globs(
    name: &str,
    binary: &str,
    category: &str,
    arguments: Vec<String>,
    path_globs: Vec<String>,
) -> &'static Tool {
    Box::leak(Box::new(Tool {
        name: name.to_string(),
        binary: binary.to_string(),
        categories: vec![category.to_string()],
        languages: vec!["yaml".to_string()],
        commands: BTreeMap::from([(
            String::new(),
            CatalogCommand {
                arguments,
                stdin: false,
            },
        )]),
        homepage: String::new(),
        path_globs,
    }))
}

/// A tool with `path_globs` must skip files that don't match and process
/// files that do match. The tool always exits non-zero so we can distinguish
/// "processed (diagnostic)" from "skipped (empty)".
#[cfg(unix)]
#[test]
fn path_globs_skips_non_matching_and_runs_matching_files() {
    let tool = leak_tool_with_globs(
        "scopedlint",
        "sh",
        "linter",
        vec!["-c".to_string(), "exit 1".to_string(), PATH_PLACEHOLDER.to_string()],
        vec!["**/.github/workflows/**/*.yml".to_string()],
    );
    let engine = lint_engine_default(tool, None, None).expect("non-mutating linter wires");

    let non_match = engine.lint(&make_src("Taskfile.yml", ""), &cfg()).unwrap();
    assert!(
        non_match.is_empty(),
        "Taskfile.yml does not match .github/workflows/**/*.yml — must be skipped; got: {non_match:?}"
    );

    let matches = engine.lint(&make_src(".github/workflows/ci.yml", ""), &cfg()).unwrap();
    assert!(
        !matches.is_empty(),
        ".github/workflows/ci.yml matches the glob — tool must run and report; got: {matches:?}"
    );
}

#[test]
fn actionlint_catalog_entry_has_github_workflows_path_globs() {
    let catalog = poly_catalog::Catalog::get();
    let tool = catalog.tool("actionlint").expect("actionlint is in the catalog");
    assert!(
        !tool.path_globs.is_empty(),
        "actionlint must declare path_globs to restrict it to workflow files"
    );
    assert!(
        tool.path_globs.iter().any(|g| g.contains(".github/workflows")),
        "actionlint path_globs must reference .github/workflows; got: {:?}",
        tool.path_globs
    );
}
