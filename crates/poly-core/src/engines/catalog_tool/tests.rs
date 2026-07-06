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
    // shellcheck is lint-only; it has no format command.
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
    // A `--fix` command would rewrite files; it must never be wired as a
    // linter, regardless of which mutating flag is present.
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
fn lint_engine_rejects_a_mutating_args_override() {
    // The guard applies to the user's `args` override too, not just the
    // catalog's own argv.
    let tool = leak_tool("fakelint", "true", "linter", vec![PATH_PLACEHOLDER.to_string()]);
    assert!(lint_engine_default(tool, None, Some(&["--fix".to_string()])).is_none());
}

#[cfg(unix)]
#[test]
fn lint_engine_reports_one_diagnostic_on_nonzero_exit() {
    // Drive the tool through an inline `sh -c` command rather than writing
    // and exec'ing a script file: exec'ing a freshly written executable can
    // transiently fail with ETXTBSY when a concurrent test thread forks
    // while this file's write fd is briefly open (CLOEXEC only closes on
    // exec, not fork). `sh -c` reaches the same stdout/stderr/exit-code
    // behaviour without ever exec'ing a file we just wrote.
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
    // Inline `sh -c` instead of exec'ing a freshly written script — see
    // `lint_engine_reports_one_diagnostic_on_nonzero_exit` for why (ETXTBSY
    // race under concurrent test threads).
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
    // A read-only linter must run against the real on-disk file (preserving
    // project context) rather than a `poly-catalog-*` temp copy. Write a real
    // file, point src at it with matching content, and assert the tool received
    // the canonical real path.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("real.txt");
    std::fs::write(&file, "hello\n").unwrap();
    let tool = leak_tool(
        "pathecho",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            // Print the path argument ($0) then fail so it surfaces as a diagnostic.
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
    // When the in-memory content differs from what's on disk (e.g. a re-lint
    // after an in-memory fix), the linter must see the content being linted —
    // so it falls back to a temp copy rather than the stale real file.
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
    // pyrefly / mypy / ty resolve imports project-wide and cannot run per file;
    // they must never be wired as catalog linters, even though the catalog lists
    // them under the `linter` category.
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
    // A genuine per-file linter (shellcheck) is not denylisted.
    assert!(!is_whole_project_linter("shellcheck"));
}

#[test]
fn absent_binary_is_a_noop() {
    // A catalog tool whose binary is essentially never installed in CI must
    // degrade to Unchanged rather than erroring.
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
    // Prove the engine forwards `env` to the subprocess. Use `sh -c` inline
    // to avoid exec'ing a freshly written file (ETXTBSY race — see above).
    let tool = leak_tool(
        "envcheck",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            // Print the env var on stdout; exit non-zero so we can capture
            // it as a diagnostic message (exit 0 yields no diagnostics).
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
    // Prove the engine sets the working directory via `root`. The tool
    // prints the cwd; we canonicalize the expected path (macOS symlinks
    // /var/folders → /private/var/folders) before comparing.
    let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    let tool = leak_tool(
        "cwdcheck",
        "sh",
        "linter",
        vec![
            "-c".to_string(),
            // Print cwd (via `pwd -P` for the physical, symlink-resolved
            // path) then exit non-zero so it surfaces as a diagnostic.
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
        vec![
            "-c".to_string(),
            // Always fail, so a non-skipped file always produces a diagnostic.
            "exit 1".to_string(),
            PATH_PLACEHOLDER.to_string(),
        ],
        vec!["**/.github/workflows/**/*.yml".to_string()],
    );
    let engine = lint_engine_default(tool, None, None).expect("non-mutating linter wires");

    // Non-matching path → skipped (no diagnostics even though tool would fail).
    let non_match = engine.lint(&make_src("Taskfile.yml", ""), &cfg()).unwrap();
    assert!(
        non_match.is_empty(),
        "Taskfile.yml does not match .github/workflows/**/*.yml — must be skipped; got: {non_match:?}"
    );

    // Matching path → tool runs → diagnostic (exit 1).
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
