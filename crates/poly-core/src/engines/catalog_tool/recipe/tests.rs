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

mod go_fix;

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
        commands.insert(
            "fix".to_owned(),
            Command {
                arguments: vec!["run".to_owned(), "--fix".to_owned(), "$DIR".to_owned()],
                stdin: false,
                then: Vec::new(),
                batch: None,
            },
        );
        commands.insert(
            "prep".to_owned(),
            Command {
                arguments: vec!["prepare".to_owned(), "$ROOT".to_owned()],
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
    let values = results[1].as_ref().expect("ok");
    assert_eq!(
        values.len(),
        1,
        "the unit-wide finding reaches every file of the run, so no verdict depends on its siblings: {values:?}"
    );
    assert!(values[0].title.starts_with("templates/other.yaml: "), "{values:?}");

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
fn a_findings_all_tool_is_keyed_to_its_root_digest_not_to_the_process() {
    let fx = Fixture::new();
    let reporter = fx.script("reporter", "#!/bin/sh\nexit 0\n");
    std::fs::write(fx.dir.path().join("Chart.yaml"), "name: c\n").expect("chart");
    let template = fx.file("templates/a.yaml", "a: 1\n");
    let tool = fx.tool(
        "lint $ROOT",
        serde_json::json!({"output": "helm_text", "group": {"marker": ["Chart.yaml"]}, "target": "root", "findings": "all"}),
        Role::Lint,
        &fx.config(reporter),
    );

    let salt = tool.cache_salt(&template.path).expect("a findings=all tool is salted");

    assert!(
        !salt.starts_with("run-"),
        "the salt is a digest, not a process nonce: {salt}"
    );
    assert_eq!(
        salt,
        tool.cache_salt(&template.path).expect("salted"),
        "stable across calls"
    );
    let other = tempfile::tempdir().expect("other chart");
    std::fs::write(other.path().join("Chart.yaml"), "name: d\n").expect("chart");
    std::fs::create_dir_all(other.path().join("templates")).expect("templates");
    std::fs::write(other.path().join("templates/a.yaml"), "a: 2\n").expect("template");
    let other_template = other.path().join("templates/a.yaml");
    let mut moved = fx.tool(
        "lint $ROOT",
        serde_json::json!({"output": "helm_text", "group": {"marker": ["Chart.yaml"]}, "target": "root", "findings": "all"}),
        Role::Lint,
        &fx.config(fx.dir.path().join("reporter").to_string_lossy().into_owned()),
    );
    moved.cwd = std::fs::canonicalize(other.path()).expect("canonical");
    assert_ne!(
        salt,
        moved.cache_salt(&other_template).expect("salted"),
        "a different chart has a different digest"
    );
}

const SILENT: &str = "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\nexit 0\n";

fn chart_recipe(extra: serde_json::Value) -> serde_json::Value {
    let mut recipe = serde_json::json!({
        "output": "helm_text",
        "group": {"marker": ["Chart.yaml"]},
        "target": "root",
        "findings": "all",
        "values_flag": "-f"
    });
    for (key, value) in extra.as_object().expect("object") {
        recipe[key] = value.clone();
    }
    recipe
}

#[test]
fn each_chart_gets_its_own_values_files_and_unverified_skipped_charts_are_not_run() {
    let fx = Fixture::new();
    let tool_bin = fx.script("helm", SILENT);
    for chart in ["apps/a", "apps/b", "lib/common"] {
        std::fs::create_dir_all(fx.dir.path().join(chart)).expect("chart dir");
        std::fs::write(fx.dir.path().join(chart).join("Chart.yaml"), "name: c\n").expect("chart");
    }
    let files = vec![
        fx.file("apps/a/values.yaml", "x: 1\n"),
        fx.file("apps/b/values.yaml", "x: 1\n"),
        fx.file("lib/common/values.yaml", "x: 1\n"),
    ];
    let root = fx.dir.path().to_string_lossy().into_owned();
    let mut config = fx.config(tool_bin);
    config.values_files = vec![format!("{root}/render/all.yaml")];
    config.chart_values = BTreeMap::from([("apps/a".to_owned(), vec![format!("{root}/render/a.yaml")])]);
    config.skip_charts = vec!["lib/*".to_owned()];
    config.verify_skip_charts = Some(false);
    let tool = fx.tool("lint $ROOT", chart_recipe(serde_json::json!({})), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(fx.invocations(), 2, "the library chart is skipped:\n{log}");
    assert!(
        log.lines()
            .any(|l| l.contains("/apps/a lint . -f ")
                && l.ends_with(&format!("render/all.yaml -f {root}/render/a.yaml"))),
        "chart a gets the shared and its own values:\n{log}"
    );
    assert!(
        log.lines()
            .any(|l| l.contains("/apps/b lint . -f ") && l.ends_with("render/all.yaml")),
        "chart b gets only the shared values:\n{log}"
    );
    assert!(results.iter().all(|r| r.as_ref().is_ok_and(Vec::is_empty)));
}

#[test]
fn values_files_are_refused_for_a_tool_whose_recipe_has_no_values_flag() {
    let fx = Fixture::new();
    let mut config = fx.config(fx.script("tool", SILENT));
    config.values_files = vec!["values.yaml".to_owned()];
    let recipe: &'static Recipe = Box::leak(Box::new(
        serde_json::from_value(serde_json::json!({"output": "helm_text"})).expect("recipe"),
    ));
    let tool: &'static Tool = Box::leak(Box::new(Tool {
        name: "faketool".to_owned(),
        binary: "faketool".to_owned(),
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
        .expect("must refuse");
    assert!(error.contains("values_files"), "{error}");
}

#[test]
fn a_chart_with_missing_dependencies_is_prepared_once_before_it_is_linted() {
    let fx = Fixture::new();
    let tool_bin = fx.script("helm", SILENT);
    std::fs::create_dir_all(fx.dir.path().join("needy")).expect("needy");
    std::fs::write(
        fx.dir.path().join("needy/Chart.yaml"),
        "name: needy\ndependencies:\n  - name: redis\n    version: 1.0.0\n    repository: oci://x\n",
    )
    .expect("needy chart");
    std::fs::create_dir_all(fx.dir.path().join("plain")).expect("plain");
    std::fs::write(fx.dir.path().join("plain/Chart.yaml"), "name: plain\n").expect("plain chart");
    let files = vec![
        fx.file("needy/values.yaml", "a: 1\n"),
        fx.file("plain/values.yaml", "a: 1\n"),
    ];
    let tool = fx.tool(
        "lint $ROOT",
        chart_recipe(serde_json::json!({"prepare": "prep"})),
        Role::Lint,
        &fx.config(tool_bin),
    );

    tool.lint_files(&refs(&files));
    tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    let prepares: Vec<&str> = log.lines().filter(|l| l.contains(" prepare .")).collect();
    assert_eq!(prepares.len(), 1, "once, for the chart that needs it:\n{log}");
    assert!(prepares[0].contains("/needy "), "{log}");
    let first_lint = log.lines().position(|l| l.contains("/needy lint")).expect("linted");
    let prepare = log.lines().position(|l| l.contains(" prepare .")).expect("prepared");
    assert!(prepare < first_lint, "the build precedes the lint:\n{log}");
}

#[test]
fn a_failing_prepare_step_does_not_stop_the_lint() {
    let fx = Fixture::new();
    let tool_bin = fx.script(
        "helm",
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\ncase \"$1\" in prepare) echo 'no network' >&2; exit 1;; esac\nexit 0\n",
    );
    std::fs::create_dir_all(fx.dir.path().join("needy")).expect("needy");
    std::fs::write(
        fx.dir.path().join("needy/Chart.yaml"),
        "name: needy\ndependencies:\n  - name: redis\n    version: 1.0.0\n",
    )
    .expect("chart");
    let files = vec![fx.file("needy/values.yaml", "a: 1\n")];
    let mut config = fx.config(tool_bin);
    config.prepare_backoff_secs = Some(0);
    config.prepare_jitter_secs = Some(0);
    let tool = fx.tool(
        "lint $ROOT",
        chart_recipe(serde_json::json!({"prepare": "prep"})),
        Role::Lint,
        &config,
    );

    let results = tool.lint_files(&refs(&files));

    assert!(results[0].as_ref().is_ok_and(Vec::is_empty), "{:?}", results[0]);
    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert!(log.contains("needy lint"), "the lint still ran:\n{log}");
    let prepares = log.lines().filter(|l| l.contains(" prepare .")).count();
    assert_eq!(prepares, 5, "five attempts by default:\n{log}");
}

const FLAKY_PREPARE: &str = "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\ncase \"$1\" in prepare) n=$(cat \"$PWD/.attempts\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$PWD/.attempts\"; if [ $n -lt 3 ]; then echo 'throttled' >&2; exit 1; fi;; esac\nexit 0\n";

fn needy_chart(fx: &Fixture) -> Vec<SourceFile> {
    std::fs::create_dir_all(fx.dir.path().join("needy")).expect("needy");
    std::fs::write(
        fx.dir.path().join("needy/Chart.yaml"),
        "name: needy\ndependencies:\n  - name: redis\n    version: 1.0.0\n",
    )
    .expect("chart");
    vec![fx.file("needy/values.yaml", "a: 1\n")]
}

#[test]
fn a_prepare_step_that_recovers_is_not_attempted_again() {
    let fx = Fixture::new();
    let files = needy_chart(&fx);
    let mut config = fx.config(fx.script("helm", FLAKY_PREPARE));
    config.prepare_backoff_secs = Some(0);
    config.prepare_jitter_secs = Some(0);
    let tool = fx.tool(
        "lint $ROOT",
        chart_recipe(serde_json::json!({"prepare": "prep"})),
        Role::Lint,
        &config,
    );

    tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(log.lines().filter(|l| l.contains(" prepare .")).count(), 3, "{log}");
}

#[test]
fn prepare_attempts_bounds_the_retries() {
    let fx = Fixture::new();
    let files = needy_chart(&fx);
    let mut config = fx.config(fx.script("helm", FLAKY_PREPARE));
    config.prepare_attempts = Some(1);
    let tool = fx.tool(
        "lint $ROOT",
        chart_recipe(serde_json::json!({"prepare": "prep"})),
        Role::Lint,
        &config,
    );

    let results = tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(log.lines().filter(|l| l.contains(" prepare .")).count(), 1, "{log}");
    assert!(results[0].as_ref().is_ok_and(Vec::is_empty), "{:?}", results[0]);
}

fn library_chart(fx: &Fixture) -> Vec<SourceFile> {
    std::fs::create_dir_all(fx.dir.path().join("lib/common")).expect("lib");
    std::fs::write(fx.dir.path().join("lib/common/Chart.yaml"), "name: common\n").expect("chart");
    vec![fx.file("lib/common/values.yaml", "x: 1\n")]
}

#[test]
fn a_skipped_chart_that_now_succeeds_fails_the_run() {
    let fx = Fixture::new();
    let files = library_chart(&fx);
    let mut config = fx.config(fx.script("helm", SILENT));
    config.skip_charts = vec!["lib/*".to_owned()];
    let tool = fx.tool("lint $ROOT", chart_recipe(serde_json::json!({})), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    let error = results[0].as_ref().expect_err("a stale skip is a failure").to_string();
    assert!(error.contains("skip_charts") && error.contains("lib/common"), "{error}");
    assert_eq!(fx.invocations(), 1, "the skipped chart is run once, to check it");
}

#[test]
fn a_skipped_chart_that_still_fails_stays_skipped() {
    let fx = Fixture::new();
    let files = library_chart(&fx);
    let failing =
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\necho 'library charts cannot be rendered' >&2\nexit 1\n";
    let mut config = fx.config(fx.script("helm", failing));
    config.skip_charts = vec!["lib/*".to_owned()];
    let tool = fx.tool("lint $ROOT", chart_recipe(serde_json::json!({})), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    assert!(results[0].as_ref().is_ok_and(Vec::is_empty), "{:?}", results[0]);
    assert_eq!(fx.invocations(), 1);
}

#[test]
fn verify_skip_charts_false_never_runs_a_skipped_chart() {
    let fx = Fixture::new();
    let files = library_chart(&fx);
    let mut config = fx.config(fx.script("helm", SILENT));
    config.skip_charts = vec!["lib/*".to_owned()];
    config.verify_skip_charts = Some(false);
    let tool = fx.tool("lint $ROOT", chart_recipe(serde_json::json!({})), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    assert!(results[0].as_ref().is_ok_and(Vec::is_empty), "{:?}", results[0]);
    assert_eq!(fx.invocations(), 0);
}

const GO_REPORTER: &str = r#"#!/bin/sh
echo "$PWD GOOS=$GOOS $*" >> "$POLY_TEST_LOG"
printf '{"Issues":[{"FromLinter":"common","Text":"everywhere","Pos":{"Filename":"a.go","Line":1,"Column":1}},{"FromLinter":"tag","Text":"only %s","Pos":{"Filename":"a.go","Line":2,"Column":1}}]}' "$GOOS"
"#;

fn go_module(fx: &Fixture) -> Vec<SourceFile> {
    std::fs::write(fx.dir.path().join("go.mod"), "module x\n").expect("go.mod");
    vec![fx.file("a.go", "package a\n")]
}

fn go_recipe() -> serde_json::Value {
    serde_json::json!({
        "output": "golangci_json",
        "group": {"marker": ["go.mod"]},
        "target": "dir",
        "new_from_rev_flag": "--new-from-rev"
    })
}

#[test]
fn new_from_rev_becomes_the_tools_flag_before_the_package() {
    let fx = Fixture::new();
    let files = go_module(&fx);
    let mut config = fx.config(fx.script("golangci", GO_REPORTER));
    config.new_from_rev = Some("HEAD".to_owned());
    let tool = fx.tool("run $DIR", go_recipe(), Role::Lint, &config);

    tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert!(log.contains(" run --new-from-rev=HEAD ."), "{log}");
}

#[test]
fn new_from_rev_changes_the_cache_identity() {
    let fx = Fixture::new();
    let bin = fx.script("golangci", GO_REPORTER);
    let plain = fx.tool("run $DIR", go_recipe(), Role::Lint, &fx.config(bin.clone()));
    let mut config = fx.config(bin);
    config.new_from_rev = Some("HEAD".to_owned());
    let scoped = fx.tool("run $DIR", go_recipe(), Role::Lint, &config);
    assert_ne!(plain.version(), scoped.version());
}

#[test]
fn new_from_rev_is_refused_by_a_tool_without_the_flag() {
    let fx = Fixture::new();
    let mut config = fx.config(fx.script("tool", SILENT));
    config.new_from_rev = Some("HEAD".to_owned());
    let recipe: &'static Recipe = Box::leak(Box::new(Recipe::default()));
    let tool: &'static Tool = Box::leak(Box::new(Tool {
        name: "faketool".to_owned(),
        binary: "faketool".to_owned(),
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
        .expect("must refuse");
    assert!(error.contains("new_from_rev"), "{error}");
}

#[test]
fn a_goos_matrix_runs_once_per_os_and_merges_the_findings() {
    let fx = Fixture::new();
    let files = go_module(&fx);
    let mut config = fx.config(fx.script("golangci", GO_REPORTER));
    config.goos = vec!["linux".to_owned(), "windows".to_owned()];
    let tool = fx.tool("run $DIR", go_recipe(), Role::Lint, &config);

    let results = tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(fx.invocations(), 2, "{log}");
    assert!(log.contains("GOOS=linux") && log.contains("GOOS=windows"), "{log}");
    let mut titles: Vec<String> = results[0]
        .as_ref()
        .expect("ok")
        .iter()
        .map(|d| d.title.clone())
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        ["everywhere", "only linux", "only windows"],
        "shared findings are not doubled"
    );
}

#[test]
fn editing_the_config_file_a_tool_discovers_changes_the_cache_identity() {
    let fx = Fixture::new();
    let linter = fx.script("linter", LINTER);
    let module = fx.dir.path().join("module");
    std::fs::create_dir_all(&module).expect("module");
    std::fs::write(module.join("buf.yaml"), "version: v2\nlint:\n  use: [STANDARD]\n").expect("buf.yaml");
    let recipe = serde_json::json!({"output": "buf_json", "config_files": ["buf.yaml"]});
    let mut config = fx.config(linter);
    config.root = Some(module.to_string_lossy().into_owned());

    let before = fx.tool("lint $FILES:--path", recipe.clone(), Role::Lint, &config);
    let unchanged = fx.tool("lint $FILES:--path", recipe.clone(), Role::Lint, &config);
    std::fs::write(module.join("buf.yaml"), "version: v2\nlint:\n  use: [MINIMAL]\n").expect("edit buf.yaml");
    let after = fx.tool("lint $FILES:--path", recipe, Role::Lint, &config);

    assert_eq!(before.version(), unchanged.version());
    assert_ne!(
        before.version(),
        after.version(),
        "a changed buf.yaml must not serve cached verdicts"
    );
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

    let before = tool.cache_salt(&src.path).expect("salted");
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
        tool.cache_salt(&src.path).expect("memoized"),
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

#[test]
fn a_version_pin_that_cannot_be_honoured_never_falls_back_to_the_path_binary() {
    let recipe: &'static Recipe = Box::leak(Box::new(
        serde_json::from_value(serde_json::json!({
            "install": {"hint": "brew install sh", "releases": {"1.0": {"kind": "binary", "assets": {"any": {
                "url": "http://127.0.0.1:1/never-1.0", "sha256": "00"
            }}}}}
        }))
        .expect("recipe"),
    ));
    let tool: &'static Tool = Box::leak(Box::new(Tool {
        name: "faketool".to_owned(),
        binary: "sh".to_owned(),
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

    let unpinned = RecipeTool::build(tool, recipe, &ToolConfig::default(), Role::Lint, None);
    assert!(unpinned.is_ok(), "`sh` is on PATH, so an unpinned tool runs");

    let pinned = ToolConfig {
        version: Some("1.0".to_owned()),
        ..ToolConfig::default()
    };
    let error = RecipeTool::build(tool, recipe, &pinned, Role::Lint, None)
        .err()
        .expect("a pin that is not installed must not run the PATH binary");
    assert!(error.contains("pinned to version 1.0"), "{error}");
    assert!(error.contains("will not run a different"), "{error}");
    assert!(error.contains("install = \"auto\""), "names the remedy: {error}");
}

#[test]
fn go_package_directories_follow_the_dot_dot_dot_pattern() {
    let fx = Fixture::new();
    let tool_bin = fx.script(
        "golint",
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\nprintf '{\"Issues\":[]}'\n",
    );
    std::fs::write(fx.dir.path().join("go.mod"), "module x\n").expect("go.mod");
    let files = vec![
        fx.file("pkg/a.go", "package a\n"),
        fx.file("pkg/b.go", "package a\n"),
        fx.file("testdata/c.go", "package c\n"),
        fx.file("_scratch/d.go", "package d\n"),
    ];
    let tool = fx.tool(
        "run $DIR",
        serde_json::json!({"output": "golangci_json", "group": {"marker": ["go.mod"]}, "target": "dir"}),
        Role::Lint,
        &fx.config(tool_bin),
    );

    let results = tool.lint_files(&refs(&files));

    let log = std::fs::read_to_string(&fx.log).expect("log");
    assert_eq!(fx.invocations(), 1, "one module, one run:\n{log}");
    assert!(
        log.trim_end().ends_with("run ./pkg"),
        "only the package directory is passed: {log}"
    );
    assert!(results.iter().all(|r| r.as_ref().is_ok_and(Vec::is_empty)));
}

#[test]
fn findings_printed_relative_to_a_chdir_reach_the_files_in_that_directory() {
    let fx = Fixture::new();
    let reporter = fx.script(
        "reporter",
        "#!/bin/sh\necho \"$PWD $*\" >> \"$POLY_TEST_LOG\"\necho 'user.tf:3: boom'\nexit 1\n",
    );
    let files = vec![fx.file("infra/user.tf", "x\n"), fx.file("other/user.tf", "y\n")];
    let tool = fx.tool(
        "--chdir=$DIR",
        serde_json::json!({"output": "path_line", "group": "dir", "target": "dir"}),
        Role::Lint,
        &fx.config(reporter),
    );

    let results = tool.lint_files(&refs(&files));

    assert_eq!(fx.invocations(), 2, "one invocation per directory");
    for result in &results {
        let diagnostics = result.as_ref().expect("ok");
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0].title, "boom");
        assert_eq!(diagnostics[0].span.map(|s| s.start_line), Some(3));
    }
}
