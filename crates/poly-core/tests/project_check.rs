//! The project-check phase end to end: a synthetic whole-project engine is registered through the
//! runner's test seam and driven by `lint_run`, proving that findings land on the right files, a
//! second identical run is served from the cache, and inline `poly: allow` directives and
//! `[per-file-ignores]` apply to project findings exactly as they do to per-file ones.
#![cfg(feature = "all-engines")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use poly_core::engine::{ProjectRequest, Severity, Span};
use poly_core::runner::{SuppressionReason, with_extra_project_engines};
use poly_core::{Capabilities, Config, Diagnostic, Engine, Language, LintRun, RunOptions, lint_run};

const ENGINE: &str = "synthetic-project";
const CODE: &str = "PRJ001";
/// A source line the synthetic engine reports on.
const MARKER: &str = "FLAG_ME";

/// Reports [`CODE`] on every Python line containing [`MARKER`], and counts how often it ran.
struct Synthetic {
    calls: Arc<AtomicUsize>,
}

impl Engine for Synthetic {
    fn name(&self) -> &'static str {
        ENGINE
    }

    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            project: true,
            ..Capabilities::default()
        }
    }

    fn version(&self) -> &str {
        "1"
    }

    fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut reported = Vec::new();
        for file in req.files {
            let diagnostics: Vec<Diagnostic> = file
                .content
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(MARKER))
                .map(|(index, _)| finding(index as u32 + 1))
                .collect();
            if !diagnostics.is_empty() {
                reported.push((file.path.clone(), diagnostics));
            }
        }
        Ok(reported)
    }
}

fn finding(line: u32) -> Diagnostic {
    Diagnostic {
        engine: ENGINE.to_owned(),
        code: Some(CODE.to_owned()),
        severity: Severity::Warning,
        title: "flagged by the project check".to_owned(),
        description: None,
        span: Some(Span {
            start_line: line,
            start_col: 1,
            end_line: line,
            end_col: 2,
        }),
        url: None,
        fix: Vec::new(),
        metadata: BTreeMap::new(),
    }
}

fn write(root: &Path, name: &str, contents: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn opts(no_cache: bool) -> RunOptions {
    RunOptions {
        no_cache,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

/// Run `body` with the synthetic engine registered and a private cache directory.
fn with_engine<R>(body: impl FnOnce(&Arc<AtomicUsize>) -> R) -> R {
    let calls = Arc::new(AtomicUsize::new(0));
    let engine: Arc<dyn Engine> = Arc::new(Synthetic { calls: calls.clone() });
    with_extra_project_engines(vec![engine], || {
        let cache_home = tempfile::tempdir().unwrap();
        // SAFETY: `with_extra_project_engines` serialises every caller in this process, and this
        // binary starts no other thread that reads the environment while `body` runs.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        body(&calls)
    })
}

/// Every `(path, code)` pair the synthetic engine reported, sorted.
fn project_findings(run: &LintRun) -> Vec<(PathBuf, String)> {
    let mut found: Vec<_> = run
        .results
        .iter()
        .flat_map(|result| {
            result
                .diagnostics
                .iter()
                .filter(|d| d.engine == ENGINE)
                .map(|d| (result.path.clone(), d.code.clone().unwrap_or_default()))
        })
        .collect();
    found.sort();
    found
}

fn lint(root: &Path, config: &Config, no_cache: bool) -> LintRun {
    lint_run(&[root.to_path_buf()], config, &opts(no_cache), false, false).unwrap()
}

#[test]
fn findings_land_on_the_files_they_name() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        let flagged = write(dir.path(), "src/a.py", "x = 1\nFLAG_ME = 2\n");
        write(dir.path(), "src/clean.py", "y = 1\n");
        let run = lint(dir.path(), &Config::default(), true);
        assert_eq!(project_findings(&run), vec![(flagged.clone(), CODE.to_owned())]);
        let result = run.results.iter().find(|r| r.path == flagged).unwrap();
        let diagnostic = result.diagnostics.iter().find(|d| d.engine == ENGINE).unwrap();
        assert_eq!(diagnostic.span.unwrap().start_line, 2);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one project check per run, not per file"
        );
    });
}

#[test]
fn second_identical_run_is_a_cache_hit() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        let flagged = write(dir.path(), "a.py", "FLAG_ME = 1\n");
        let first = lint(dir.path(), &Config::default(), false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let second = lint(dir.path(), &Config::default(), false);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the engine must not run again on unchanged input"
        );
        assert_eq!(project_findings(&first), project_findings(&second));
        assert_eq!(project_findings(&second), vec![(flagged.clone(), CODE.to_owned())]);

        write(dir.path(), "a.py", "FLAG_ME = 1\nz = 3\n");
        lint(dir.path(), &Config::default(), false);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a changed file invalidates the entry");
    });
}

#[test]
fn no_cache_runs_the_engine_every_time() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.py", "FLAG_ME = 1\n");
        lint(dir.path(), &Config::default(), true);
        lint(dir.path(), &Config::default(), true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn inline_allow_directive_suppresses_a_project_finding() {
    with_engine(|_| {
        let dir = tempfile::tempdir().unwrap();
        let allowed = write(
            dir.path(),
            "allowed.py",
            &format!("# poly: allow[{CODE}] flagged on purpose\n{MARKER} = 1\n"),
        );
        let flagged = write(dir.path(), "flagged.py", &format!("{MARKER} = 1\n"));
        let run = lint(dir.path(), &Config::default(), true);
        assert_eq!(project_findings(&run), vec![(flagged, CODE.to_owned())]);
        let record = run
            .suppressed
            .iter()
            .find(|s| s.path == allowed)
            .expect("the suppressed project finding is recorded");
        assert_eq!(record.code.as_deref(), Some(CODE));
        assert_eq!(record.reason, SuppressionReason::InlineSuppression);
    });
}

#[test]
fn suppression_still_applies_when_the_result_is_replayed_from_cache() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "allowed.py",
            &format!("# poly: allow[{CODE}] flagged on purpose\n{MARKER} = 1\n"),
        );
        lint(dir.path(), &Config::default(), false);
        let replay = lint(dir.path(), &Config::default(), false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(project_findings(&replay).is_empty());
    });
}

#[test]
fn per_file_ignores_apply_to_project_findings() {
    with_engine(|_| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "legacy/old.py", &format!("{MARKER} = 1\n"));
        let kept = write(dir.path(), "src/new.py", &format!("{MARKER} = 1\n"));
        let mut per_file_ignores = BTreeMap::new();
        per_file_ignores.insert("legacy/**".to_string(), vec![CODE.to_string()]);
        let config = Config {
            per_file_ignores,
            ..Config::default()
        };
        let run = lint(dir.path(), &config, true);
        assert_eq!(project_findings(&run), vec![(kept, CODE.to_owned())]);
        assert!(run.suppressed.iter().any(|s| s.path.ends_with("legacy/old.py")));
    });
}

#[test]
fn only_selection_that_excludes_the_engine_skips_the_phase() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.py", &format!("{MARKER} = 1\n"));
        let options = RunOptions {
            only: vec!["ruff".to_string()],
            ..opts(true)
        };
        let run = lint_run(&[dir.path().to_path_buf()], &Config::default(), &options, false, false).unwrap();
        assert!(project_findings(&run).is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn disabled_engine_does_not_run() {
    with_engine(|calls| {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.py", &format!("{MARKER} = 1\n"));
        let mut engine_table = toml::Table::new();
        engine_table.insert("enabled".to_string(), toml::Value::Boolean(false));
        let mut python = toml::Table::new();
        python.insert(ENGINE.to_string(), toml::Value::Table(engine_table));
        let mut lint_table = toml::Table::new();
        lint_table.insert("python".to_string(), toml::Value::Table(python));
        let config = Config {
            lint: lint_table,
            ..Config::default()
        };
        let run = lint(dir.path(), &config, true);
        assert!(project_findings(&run).is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    });
}

/// The real ty engine through `lint_run`: off until `backend` is set, then its findings land on
/// the file like any other diagnostic.
#[test]
fn python_types_runs_only_with_a_backend() {
    let dir = tempfile::tempdir().unwrap();
    let cache_home = tempfile::tempdir().unwrap();
    with_extra_project_engines(Vec::new(), || {
        // SAFETY: `with_extra_project_engines` serialises every caller in this process.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        write(dir.path(), "app.py", "count: int = \"three\"\n");
        let codes = |run: &LintRun| -> Vec<String> {
            run.results
                .iter()
                .flat_map(|r| &r.diagnostics)
                .filter(|d| d.engine == "python-types")
                .filter_map(|d| d.code.clone())
                .collect()
        };

        let off = lint(dir.path(), &Config::default(), true);
        assert!(codes(&off).is_empty(), "{:?}", off.results);

        let lint_table: toml::Table = toml::from_str("[python.python-types]\nbackend = \"ty\"\n").unwrap();
        let config = Config {
            lint: lint_table,
            ..Config::default()
        };
        let on = lint(dir.path(), &config, true);
        assert_eq!(codes(&on), ["invalid-assignment"]);
    });
}
