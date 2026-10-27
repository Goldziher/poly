//! End-to-end behaviour of a recipe tool, driven by a fake tool script so the
//! assertions are about poly (spawn counts, attribution, isolation) and not about
//! any real tool.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use poly_catalog::{Command, Recipe, Tool};
use poly_config::ToolConfig;

use super::{RecipeTool, Role};
use crate::engine::{FormatOutput, Severity, SourceFile};
use crate::language::Language;

/// A buf-flavoured linter: `lint --path f ...` prints one JSON object per file
/// whose content contains BAD, exits 2 with no report if a file contains CRASH,
/// and logs `<cwd> <args>` per invocation.
const LINTER: &str = r#"#!/bin/sh
echo "$PWD $*" >> "$POLY_TEST_LOG"
for arg in "$@"; do
  [ -f "$arg" ] || continue
  if grep -q CRASH "$arg"; then echo "boom" >&2; exit 2; fi
done
for arg in "$@"; do
  [ -f "$arg" ] || continue
  if grep -q BAD "$arg"; then
    printf '{"path":"%s","start_line":2,"start_column":1,"end_line":2,"end_column":4,"type":"RULE","message":"bad"}\n' "$arg"
  fi
done
exit 0
"#;

/// An in-place upper-caser: `<files...>` rewritten with `tr`; exits 1 when a file changed.
const UPPERCASER: &str = r#"#!/bin/sh
echo "$PWD $*" >> "$POLY_TEST_LOG"
status=0
for arg in "$@"; do
  [ -f "$arg" ] || continue
  tr a-z A-Z < "$arg" > "$arg.tmp"
  if ! cmp -s "$arg" "$arg.tmp"; then status=1; fi
  mv "$arg.tmp" "$arg"
done
exit $status
"#;

struct Fixture {
    dir: tempfile::TempDir,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("invocations.log");
        std::fs::write(&log, "").expect("log");
        Fixture { dir, log }
    }

    fn script(&self, name: &str, body: &str) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, body).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path.to_string_lossy().into_owned()
    }

    fn file(&self, rel: &str, content: &str) -> SourceFile {
        let path = self.dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, content).expect("write");
        SourceFile {
            path,
            language: Language::Proto,
            content: Arc::from(content),
        }
    }

    fn invocations(&self) -> usize {
        std::fs::read_to_string(&self.log).expect("log").lines().count()
    }

    fn config(&self, bin: String) -> ToolConfig {
        let mut config = ToolConfig {
            bin: Some(bin),
            ..ToolConfig::default()
        };
        config
            .env
            .insert("POLY_TEST_LOG".to_owned(), self.log.to_string_lossy().into_owned());
        config
    }

    /// Build a tool whose working directory is the fixture, so relative paths resolve there.
    fn tool(&self, command: &str, recipe: serde_json::Value, role: Role, config: &ToolConfig) -> RecipeTool {
        let mut commands = BTreeMap::new();
        commands.insert(
            "run".to_owned(),
            Command {
                arguments: serde_json::from_value(serde_json::json!(command.split(' ').collect::<Vec<_>>()))
                    .expect("argv"),
                stdin: false,
                then: Vec::new(),
                batch: None,
            },
        );
        let recipe: Recipe = serde_json::from_value(recipe).expect("recipe");
        let tool: &'static Tool = Box::leak(Box::new(Tool {
            name: "faketool".to_owned(),
            binary: "faketool".to_owned(),
            categories: vec!["linter".to_owned(), "formatter".to_owned()],
            languages: vec!["proto".to_owned()],
            commands,
            homepage: String::new(),
            path_globs: Vec::new(),
            recipe: Some(recipe.clone()),
        }));
        let recipe: &'static Recipe = tool.recipe.as_ref().expect("recipe");
        let mut built = RecipeTool::build(tool, recipe, config, role, Some("run")).expect("builds");
        built.cwd = std::fs::canonicalize(self.dir.path()).expect("canonical");
        built
    }
}

fn refs(files: &[SourceFile]) -> Vec<&SourceFile> {
    files.iter().collect()
}

#[test]
fn many_files_share_one_spawn_and_each_gets_its_own_findings() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    let files: Vec<SourceFile> = (0..6)
        .map(|i| {
            fx.file(
                &format!("p/f{i}.proto"),
                if i % 2 == 1 { "ok\nBAD\n" } else { "ok\nfine\n" },
            )
        })
        .collect();
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json"}),
        Role::Lint,
        &fx.config(linter),
    );

    let results = tool.lint_files(&refs(&files));

    assert_eq!(fx.invocations(), 1, "six files, one process");
    for (i, result) in results.iter().enumerate() {
        let diagnostics = result.as_ref().expect("no failure");
        if i % 2 == 1 {
            assert_eq!(diagnostics.len(), 1, "file {i}");
            assert_eq!(diagnostics[0].code.as_deref(), Some("RULE"));
            assert_eq!(diagnostics[0].severity, Severity::Error);
            assert_eq!(
                diagnostics[0].span.map(|s| (s.start_line, s.start_col, s.end_col)),
                Some((2, 1, 4))
            );
        } else {
            assert!(diagnostics.is_empty(), "file {i} is clean");
        }
    }
}

#[test]
fn one_file_the_tool_chokes_on_is_isolated_and_the_rest_still_pass() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    let mut files: Vec<SourceFile> = (0..8).map(|i| fx.file(&format!("f{i}.proto"), "ok\nBAD\n")).collect();
    files[5] = fx.file("f5.proto", "CRASH\n");
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json"}),
        Role::Lint,
        &fx.config(linter),
    );

    let results = tool.lint_files(&refs(&files));

    for (i, result) in results.iter().enumerate() {
        if i == 5 {
            let error = result.as_ref().expect_err("the crashing file is an error, never clean");
            assert!(error.to_string().contains("boom"), "{error:#}");
        } else {
            assert_eq!(
                result.as_ref().expect("others succeed").len(),
                1,
                "file {i} keeps its finding"
            );
        }
    }
    assert!(
        fx.invocations() <= 1 + 2 * 4,
        "halving costs O(log n) spawns, got {}",
        fx.invocations()
    );
}

#[test]
fn a_tool_that_refuses_every_invocation_is_not_retried_per_file() {
    let fx = Fixture::new();
    let linter = fx.script(
        "always-fails",
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\necho 'unknown flag' >&2\nexit 2\n",
    );
    let files: Vec<SourceFile> = (0..32).map(|i| fx.file(&format!("f{i}.proto"), "ok\n")).collect();
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json"}),
        Role::Lint,
        &fx.config(linter),
    );

    let results = tool.lint_files(&refs(&files));

    assert!(
        results.iter().all(Result::is_err),
        "a failing tool never reads as clean"
    );
    assert!(
        fx.invocations() < 12,
        "gave up after the first refusals, ran {}",
        fx.invocations()
    );
}

#[test]
fn files_are_grouped_by_project_root_and_run_once_per_root() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    std::fs::create_dir_all(fx.dir.path().join("a")).expect("a");
    std::fs::create_dir_all(fx.dir.path().join("b")).expect("b");
    std::fs::write(fx.dir.path().join("a/unit.toml"), "").expect("marker a");
    std::fs::write(fx.dir.path().join("b/unit.toml"), "").expect("marker b");
    let files = vec![
        fx.file("a/x.proto", "ok\n"),
        fx.file("a/sub/y.proto", "ok\nBAD\n"),
        fx.file("b/z.proto", "ok\n"),
        fx.file("loose.proto", "ok\nBAD\n"),
    ];
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json", "group": {"marker": ["unit.toml"]}}),
        Role::Lint,
        &fx.config(linter),
    );

    let results = tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(fx.invocations(), 2, "two roots, two spawns:\n{log}");
    assert!(
        log.lines()
            .any(|l| l.contains("/a lint --path x.proto --path sub/y.proto")),
        "{log}"
    );
    assert!(log.lines().any(|l| l.contains("/b lint --path z.proto")), "{log}");
    assert_eq!(
        results[1].as_ref().expect("ok").len(),
        1,
        "the finding lands on sub/y.proto"
    );
    assert!(results[0].as_ref().expect("ok").is_empty());
    assert!(
        results[3].as_ref().expect("ok").is_empty(),
        "a file under no root is not run"
    );
}

#[test]
fn whole_unit_findings_in_files_outside_the_run_reach_the_files_that_are() {
    let fx = Fixture::new();
    let reporter = fx.script(
        "reporter",
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\nprintf '[ERROR] templates/other.yaml: broke\\n[ERROR] templates/mine.yaml: mine\\n'\n",
    );
    std::fs::write(fx.dir.path().join("Chart.yaml"), "name: c\n").expect("chart");
    let files = vec![
        fx.file("templates/mine.yaml", "a: 1\n"),
        fx.file("values.yaml", "b: 2\n"),
    ];
    let all = fx.tool(
        "lint $ROOT",
        serde_json::json!({"output": "helm_text", "group": {"marker": ["Chart.yaml"]}, "target": "root", "findings": "all"}),
        Role::Lint,
        &fx.config(reporter.clone()),
    );
    let results = all.lint_files(&refs(&files));
    assert_eq!(
        fx.invocations(),
        1,
        "a chart is linted once however many of its files changed"
    );
    let mine = results[0].as_ref().expect("ok");
    assert_eq!(mine.len(), 2, "its own finding plus the unit-wide one: {mine:?}");
    assert!(
        mine.iter().any(|d| d.title.starts_with("templates/other.yaml: ")),
        "{mine:?}"
    );
    assert_eq!(
        results[1].as_ref().expect("ok").len(),
        1,
        "values.yaml gets the unit-wide finding too"
    );

    let own = fx.tool(
        "lint $ROOT",
        serde_json::json!({"output": "helm_text", "group": {"marker": ["Chart.yaml"]}, "target": "root", "findings": "own"}),
        Role::Lint,
        &fx.config(reporter),
    );
    let results = own.lint_files(&refs(&files));
    assert_eq!(
        results[0].as_ref().expect("ok").len(),
        1,
        "own: only findings in the run's files"
    );
    assert!(results[1].as_ref().expect("ok").is_empty());
}

#[test]
fn content_that_differs_from_disk_is_linted_from_a_scratch_copy() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    let mut src = fx.file("f.proto", "ok\n");
    src.content = Arc::from("ok\nBAD\n");
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json"}),
        Role::Lint,
        &fx.config(linter),
    );

    let results = tool.lint_files(&[&src]);

    assert_eq!(
        results[0].as_ref().expect("ok").len(),
        1,
        "the in-memory text is what is judged"
    );
    assert_eq!(
        std::fs::read_to_string(&src.path).expect("disk"),
        "ok\n",
        "the real file is untouched"
    );
}

#[test]
fn formatting_batches_rewrites_a_scratch_copy_and_leaves_disk_alone() {
    let fx = Fixture::new();
    let upper = fx.script("upper", UPPERCASER);
    let config = fx.config(upper);
    let files = vec![
        fx.file("a/x.txt", "hello\n"),
        fx.file("a/y.txt", "ALREADY\n"),
        fx.file("z.txt", "bye\n"),
    ];
    let tool = fx.tool("$FILES", serde_json::json!({"ok_exits": [1]}), Role::Format, &config);

    let results = tool.format_files(&refs(&files));

    assert_eq!(fx.invocations(), 1);
    assert!(matches!(&results[0], Ok(FormatOutput::Formatted(text)) if text == "HELLO\n"));
    assert!(matches!(&results[1], Ok(FormatOutput::Unchanged)));
    assert!(matches!(&results[2], Ok(FormatOutput::Formatted(text)) if text == "BYE\n"));
    assert_eq!(std::fs::read_to_string(&files[0].path).expect("disk"), "hello\n");
}

#[test]
fn the_cache_salt_follows_the_dependencies_of_the_files_project() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    std::fs::write(fx.dir.path().join("unit.toml"), "").expect("marker");
    let src = fx.file("a.proto", "ok\n");
    let sibling = fx.dir.path().join("b.proto");
    std::fs::write(&sibling, "message B {}").expect("sibling");
    let tool = fx.tool(
        "lint $FILES:--path",
        serde_json::json!({"output": "buf_json", "group": {"marker": ["unit.toml"]}, "dependencies": ["**/*.proto"]}),
        Role::Lint,
        &fx.config(linter),
    );

    let before = tool.cache_salt(&src).expect("salted");
    std::fs::write(&sibling, "message B { int32 x = 1; }").expect("edit sibling");
    // The digest is memoized per process, so a fresh root is needed to observe a change.
    let other = tempfile::tempdir().expect("other");
    std::fs::write(other.path().join("b.proto"), "message B {}").expect("b");
    let first = super::salt::dependency_digest(other.path(), &["**/*.proto".to_owned()]);
    std::fs::write(other.path().join("b.proto"), "message C {}").expect("b2");
    let fresh = tempfile::tempdir().expect("fresh");
    std::fs::write(fresh.path().join("b.proto"), "message C {}").expect("b3");
    assert_ne!(
        first,
        super::salt::dependency_digest(fresh.path(), &["**/*.proto".to_owned()])
    );
    assert_eq!(
        before,
        tool.cache_salt(&src).expect("memoized"),
        "one answer per process"
    );
}

#[test]
fn a_missing_tool_is_a_message_with_the_remedy_not_a_silent_pass() {
    let fx = Fixture::new();
    let config = ToolConfig {
        bin: Some("/definitely/not/installed".to_owned()),
        ..ToolConfig::default()
    };
    let recipe: &'static Recipe = Box::leak(Box::new(
        serde_json::from_value(serde_json::json!({"install": {"hint": "brew install faketool"}})).expect("recipe"),
    ));
    let tool: &'static Tool = Box::leak(Box::new(Tool {
        name: "faketool".to_owned(),
        binary: "faketool-not-on-path".to_owned(),
        categories: vec!["linter".to_owned()],
        languages: Vec::new(),
        commands: BTreeMap::from([(
            "lint".to_owned(),
            Command {
                arguments: vec!["$FILES".to_owned()],
                stdin: false,
                then: Vec::new(),
                batch: None,
            },
        )]),
        homepage: String::new(),
        path_globs: Vec::new(),
        recipe: None,
    }));
    let error = RecipeTool::build(tool, recipe, &config, Role::Lint, None)
        .err()
        .expect("must not build");
    assert!(error.contains("does not exist"), "{error}");
    let _ = (fx, Path::new("."));

    let bare = ToolConfig::default();
    let error = RecipeTool::build(tool, recipe, &bare, Role::Lint, None)
        .err()
        .expect("must not build");
    assert!(
        error.contains("brew install faketool"),
        "names the install command: {error}"
    );
    assert!(error.contains("[tools.faketool]"), "names the config entry: {error}");
}
