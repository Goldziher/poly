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
use poly_core::{Capabilities, Config, Diagnostic, Edit, Engine, Language, LintRun, RunOptions, lint_run};

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
            let mut offset = 0;
            let mut diagnostics: Vec<Diagnostic> = Vec::new();
            for (index, line) in file.content.split_inclusive('\n').enumerate() {
                if let Some(at) = line.find(MARKER) {
                    let mut found = finding(index as u32 + 1);
                    // The edit deletes the marker, so a `--fix` run resolves the finding.
                    found.fix = vec![Edit {
                        start_byte: offset + at,
                        end_byte: offset + at + MARKER.len(),
                        replacement: String::new(),
                    }];
                    diagnostics.push(found);
                }
                offset += line.len();
            }
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

/// The messages of a run's errors, joined.
fn error_text(run: &LintRun) -> String {
    run.errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `[python.python-types]` table with `backend = <value>` (TOML text) and the `[components]` it runs under.
fn python_types_config(backend: &str, components: poly_config::v2::ComponentsConfig) -> Config {
    let lint: toml::Table = toml::from_str(&format!("[python.python-types]\nbackend = {backend}\n")).unwrap();
    Config {
        lint,
        components,
        ..Config::default()
    }
}

/// A `backend` that is not a string used to switch type checking off without a word; it now reaches the
/// default engine, which names the value.
#[test]
fn a_non_string_backend_is_an_error_not_a_silent_off() {
    let dir = tempfile::tempdir().unwrap();
    let cache_home = tempfile::tempdir().unwrap();
    with_extra_project_engines(Vec::new(), || {
        // SAFETY: `with_extra_project_engines` serialises every caller in this process.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        write(dir.path(), "app.py", "count: int = \"three\"\n");
        for value in ["1", "true", "[\"ty\"]"] {
            let run = lint(dir.path(), &python_types_config(value, Default::default()), true);
            let message = error_text(&run);
            assert!(
                message.contains("unknown `backend`") && message.contains(value),
                "backend = {value}: {message}"
            );
        }
    });
}

/// `backend = "pyrefly"` with the pyrefly component disabled must not leave the run type-checking nothing.
#[test]
fn a_backend_of_a_disabled_component_is_an_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let cache_home = tempfile::tempdir().unwrap();
    with_extra_project_engines(Vec::new(), || {
        // SAFETY: `with_extra_project_engines` serialises every caller in this process.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        write(dir.path(), "app.py", "count: int = \"three\"\n");
        let components = poly_config::v2::ComponentsConfig {
            disable: vec!["pyrefly".to_owned()],
            ..Default::default()
        };
        let run = lint(
            dir.path(),
            &python_types_config("\"pyrefly\"", components.clone()),
            true,
        );
        let message = error_text(&run);
        assert!(
            message.contains("`pyrefly` component") && message.contains("[components] disable"),
            "{message}"
        );

        let skipped = RunOptions {
            skip: vec!["pyrefly".to_owned()],
            ..opts(true)
        };
        let config = python_types_config("\"pyrefly\"", components);
        let run = lint_run(&[dir.path().to_path_buf()], &config, &skipped, false, false).unwrap();
        assert!(run.errors.is_empty(), "{}", error_text(&run));
    });
}

/// `--only pyrefly` under `backend = "ty"` asks for a check the config leaves nothing to run: the run says so as a
/// skip that is not a withdrawal, so `--deny-skips` / `--max-skips` count it instead of reading it as a clean pass.
#[test]
fn only_naming_an_unselected_type_checker_is_a_counted_skip() {
    let dir = tempfile::tempdir().unwrap();
    let cache_home = tempfile::tempdir().unwrap();
    with_extra_project_engines(Vec::new(), || {
        // SAFETY: `with_extra_project_engines` serialises every caller in this process.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        write(dir.path(), "app.py", "count: int = \"three\"\n");
        let only = RunOptions {
            only: vec!["pyrefly".to_owned()],
            ..opts(true)
        };
        let config = python_types_config("\"ty\"", Default::default());
        let run = lint_run(&[dir.path().to_path_buf()], &config, &only, false, false).unwrap();
        assert!(run.errors.is_empty(), "{}", error_text(&run));

        let reason = "`--only pyrefly` selects nothing here: `[python-types] backend` is \"ty\"";
        let found: Vec<_> = run.skipped.iter().filter(|s| s.reason == reason).collect();
        assert_eq!(found.len(), 1, "{:?}", run.skipped);
        // An in-memory config has no directory, so the scope is the working directory.
        assert_eq!(found[0].path, std::env::current_dir().unwrap());

        // The budget the CLI's `report_skip_budget` applies: every skip that is not a withdrawal.
        let charged = run
            .skipped
            .iter()
            .filter(|entry| !poly_core::is_withdrawal_reason(&entry.reason))
            .count();
        assert!(charged >= 1, "--deny-skips would pass: {:?}", run.skipped);
    });
}

mod tree {
    //! An engine that declares a directory tree (`project_inputs().dirs`), as adrlint does: it judges
    //! the whole tree whatever paths the run names, and the run's paths only narrow what is reported.

    use super::*;
    use poly_core::engine::{ProjectDirs, ProjectInputs, ProjectSideFile};

    const TREE: &str = "tree-check";
    /// Reported on every served file when the engine sees fewer than three of them: the false
    /// positive a path-scoped run would cause if the engine were handed only the named file.
    const GAP: &str = "TREE_GAP";
    const STRAY: &str = "TREE_STRAY";
    const SIDE: &str = "TREE_SIDE";

    struct Tree {
        calls: Arc<AtomicUsize>,
    }

    impl Engine for Tree {
        fn name(&self) -> &'static str {
            TREE
        }

        fn languages(&self) -> &'static [Language] {
            &[Language::Markdown]
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

        fn project_inputs(&self) -> ProjectInputs {
            ProjectInputs {
                gate_key: None,
                files: vec![ProjectSideFile {
                    option_key: "lock".to_owned(),
                    default_path: "tree.lock".to_owned(),
                }],
                dirs: Some(ProjectDirs {
                    dir_key: "dir".to_owned(),
                    dir_default: "log".to_owned(),
                    paths_key: Some("paths".to_owned()),
                    recursive_key: Some("recursive".to_owned()),
                    recursive_default: false,
                    whole_tree: false,
                    file_names: Vec::new(),
                    list_unserved: true,
                }),
            }
        }

        fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let is_listing =
                |file: &poly_core::engine::ProjectFile| matches!(&file.language, Language::Other(n) if n == "listing");
            let served: Vec<_> = req.files.iter().filter(|f| f.language == Language::Markdown).collect();
            let has_side = req
                .files
                .iter()
                .any(|f| matches!(&f.language, Language::Other(n) if n != "listing"));
            let mut reported = Vec::new();
            for file in req.files {
                let mut found = Vec::new();
                if is_listing(file) {
                    found.push(code(STRAY, 1));
                } else if file.language == Language::Markdown {
                    if served.len() < 3 {
                        found.push(code(GAP, 1));
                    }
                    if has_side {
                        found.push(code(SIDE, 1));
                    }
                    for (index, line) in file.content.lines().enumerate() {
                        if line.contains(MARKER) {
                            found.push(code(CODE, index as u32 + 1));
                        }
                    }
                }
                if !found.is_empty() {
                    reported.push((file.path.clone(), found));
                }
            }
            Ok(reported)
        }
    }

    fn code(code: &str, line: u32) -> Diagnostic {
        Diagnostic {
            code: Some(code.to_owned()),
            engine: TREE.to_owned(),
            ..finding(line)
        }
    }

    /// A project with a three-record log, a stray file in it, and Markdown outside it.
    fn project(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "poly.toml",
            &format!("[workspace]\nroot = true\n\n{config}"),
        );
        write(dir.path(), "log/0001.md", "# one\n");
        write(dir.path(), "log/0002.md", "# two\nFLAG_ME\n");
        write(dir.path(), "log/0003.md", "# three\n");
        write(dir.path(), "log/notes.txt", "scratch\n");
        write(dir.path(), "other/readme.md", "# readme\n");
        dir
    }

    fn engine_run<R>(body: impl FnOnce(&Arc<AtomicUsize>) -> R) -> R {
        let calls = Arc::new(AtomicUsize::new(0));
        let engine: Arc<dyn Engine> = Arc::new(Tree { calls: calls.clone() });
        with_extra_project_engines(vec![engine], || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            body(&calls)
        })
    }

    fn run(paths: &[PathBuf]) -> LintRun {
        let options = RunOptions {
            jobs: Some(1),
            ..RunOptions::default()
        };
        lint_run(paths, &Config::default(), &options, false, false).unwrap()
    }

    /// `(file name, code)` of every finding of the tree engine.
    fn found(run: &LintRun) -> Vec<(String, String)> {
        let mut found: Vec<_> = run
            .results
            .iter()
            .flat_map(|result| {
                let name = result.path.file_name().unwrap().to_string_lossy().into_owned();
                result
                    .diagnostics
                    .iter()
                    .filter(|d| d.engine == TREE)
                    .map(move |d| (name.clone(), d.code.clone().unwrap_or_default()))
            })
            .collect();
        found.sort();
        found
    }

    fn pair(name: &str, code: &str) -> (String, String) {
        (name.to_owned(), code.to_owned())
    }

    #[test]
    fn a_run_over_the_whole_project_reports_the_tree() {
        let dir = project("");
        engine_run(|calls| {
            let run = run(&[dir.path().to_path_buf()]);
            assert_eq!(found(&run), [pair("0002.md", CODE), pair("notes.txt", STRAY)]);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn naming_one_file_reports_on_it_without_a_false_verdict_about_the_tree() {
        let dir = project("");
        engine_run(|calls| {
            let run = run(&[dir.path().join("log/0002.md")]);
            // Handed only the named file the engine would see fewer than three records and report
            // `TREE_GAP`; handed the log it reports the real finding in the file and nothing else.
            assert_eq!(found(&run), [pair("0002.md", CODE)]);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn naming_a_clean_file_of_the_log_reports_nothing() {
        let dir = project("");
        engine_run(|_| {
            assert_eq!(found(&run(&[dir.path().join("log/0001.md")])), []);
        });
    }

    #[test]
    fn naming_the_directory_reports_the_listing_too() {
        let dir = project("");
        engine_run(|_| {
            let run = run(&[dir.path().join("log")]);
            assert_eq!(found(&run), [pair("0002.md", CODE), pair("notes.txt", STRAY)]);
        });
    }

    #[test]
    fn a_run_elsewhere_in_the_project_does_not_read_the_tree() {
        let dir = project("");
        engine_run(|calls| {
            let run = run(&[dir.path().join("other")]);
            assert_eq!(found(&run), []);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "the engine must not run");
        });
    }

    #[test]
    fn a_path_scoped_run_shares_the_cache_with_a_full_run() {
        let dir = project("");
        engine_run(|calls| {
            run(&[dir.path().to_path_buf()]);
            run(&[dir.path().join("log/0002.md")]);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "same inputs, same key");
        });
    }

    #[test]
    fn no_served_file_in_the_directory_and_no_side_file_means_no_run() {
        let dir = project("");
        fs::remove_file(dir.path().join("log/0001.md")).unwrap();
        fs::remove_file(dir.path().join("log/0002.md")).unwrap();
        fs::remove_file(dir.path().join("log/0003.md")).unwrap();
        engine_run(|calls| {
            let run = run(&[dir.path().to_path_buf()]);
            assert_eq!(found(&run), []);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "a listing alone is nothing to judge");
        });
    }

    #[test]
    fn a_side_file_alone_still_runs_the_engine() {
        let dir = project("");
        for name in ["0001.md", "0002.md", "0003.md"] {
            fs::remove_file(dir.path().join("log").join(name)).unwrap();
        }
        write(dir.path(), "tree.lock", "locked\n");
        engine_run(|calls| {
            run(&[dir.path().to_path_buf()]);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn a_configured_side_file_and_recursive_directories_are_honored() {
        let dir = project("[engines.tree-check]\ndir = \"custom\"\nrecursive = true\nlock = \"./locks/mine.lock\"\n");
        write(dir.path(), "custom/a/0001.md", "# a\n");
        write(dir.path(), "custom/b/0002.md", "# b\n");
        write(dir.path(), "custom/b/0003.md", "# c\n");
        write(dir.path(), "locks/mine.lock", "locked\n");
        engine_run(|_| {
            let run = run(&[dir.path().to_path_buf()]);
            let got = found(&run);
            assert!(got.contains(&pair("0001.md", SIDE)), "{got:?}");
            assert!(got.contains(&pair("0003.md", SIDE)), "{got:?}");
            assert!(got.contains(&pair("0002.md", SIDE)), "{got:?}");
            assert!(!got.contains(&pair("readme.md", SIDE)), "outside the directory");
            assert!(!got.iter().any(|(_, code)| code == GAP), "{got:?}");
        });
    }

    #[test]
    fn an_option_of_the_wrong_type_is_an_error_not_a_silent_default() {
        let dir = project("[engines.tree-check]\ndir = 3\n");
        engine_run(|calls| {
            let run = run(&[dir.path().to_path_buf()]);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let message = run
                .errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(message.contains("tree-check") && message.contains("`dir`"), "{message}");
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_lock_and_a_symlinked_directory_are_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.lock", "x\n");
        write(outside.path(), "0001.md", "# out\nFLAG_ME\n");
        let dir = project("");
        std::os::unix::fs::symlink(outside.path().join("secret.lock"), dir.path().join("tree.lock")).unwrap();
        engine_run(|_| {
            let got = found(&run(&[dir.path().to_path_buf()]));
            assert!(!got.iter().any(|(_, code)| code == SIDE), "the lock was read: {got:?}");
        });

        let linked = project("[engines.tree-check]\ndir = \"linked\"\n");
        std::os::unix::fs::symlink(outside.path(), linked.path().join("linked")).unwrap();
        engine_run(|calls| {
            run(&[linked.path().to_path_buf()]);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "nothing under a symlinked directory is read"
            );
        });
    }
}

#[test]
fn fix_commits_the_edits_a_project_finding_carries_and_drops_the_finding() {
    with_engine(|_| {
        let dir = tempfile::tempdir().unwrap();
        let flagged = write(dir.path(), "a.py", "x = 1\nFLAG_ME = 2\n");
        let run = lint_run(
            &[dir.path().to_path_buf()],
            &Config::default(),
            &opts(true),
            true,
            false,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&flagged).unwrap(), "x = 1\n = 2\n");
        assert_eq!(project_findings(&run), vec![], "the resolved finding leaves the report");
        let result = run
            .results
            .iter()
            .find(|r| r.path == flagged)
            .expect("a fixed file is kept");
        assert_eq!(result.fixed, 1);
    });
}

#[test]
fn without_fix_the_edits_are_neither_applied_nor_advertised() {
    with_engine(|_| {
        let dir = tempfile::tempdir().unwrap();
        let flagged = write(dir.path(), "a.py", "FLAG_ME = 2\n");
        let run = lint(dir.path(), &Config::default(), true);
        assert_eq!(fs::read_to_string(&flagged).unwrap(), "FLAG_ME = 2\n");
        let result = run.results.iter().find(|r| r.path == flagged).unwrap();
        assert!(result.diagnostics.iter().all(|d| d.fix.is_empty()));
        assert_eq!(result.fixed, 0);
    });
}

#[test]
fn fix_leaves_a_hash_stamped_generated_file_alone() {
    with_engine(|_| {
        let dir = tempfile::tempdir().unwrap();
        let text = "# alef:hash:a3f1c2d4e5b6a7980123456789abcdef0123456789abcdef0123456789abcdef\nFLAG_ME = 2\n";
        let generated = write(dir.path(), "gen.py", text);
        let config = Config::default();
        let mut options = opts(true);
        options.generated = Some(true);
        let run = lint_run(&[dir.path().to_path_buf()], &config, &options, true, false).unwrap();
        assert_eq!(
            fs::read_to_string(&generated).unwrap(),
            text,
            "withheld without --fix-generated"
        );
        let result = run.results.iter().find(|r| r.path == generated).unwrap();
        assert!(result.fix_withheld_generated);
        assert_eq!(project_findings(&run).len(), 1);
    });
}

mod fix_passes {
    //! `--fix` for project findings across engines and passes: every engine's edits to a file are
    //! committed together, and the report is the check's verdict on the files as `--fix` left them.

    use super::*;

    /// Flags [`Self::marker`] in Python files; the fix deletes it. A second engine on the same file.
    struct Marker {
        name: &'static str,
        marker: &'static str,
    }

    impl Engine for Marker {
        fn name(&self) -> &'static str {
            self.name
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
            let mut reported = Vec::new();
            for file in req.files {
                let found: Vec<Diagnostic> = file
                    .content
                    .match_indices(self.marker)
                    .map(|(at, _)| Diagnostic {
                        engine: self.name.to_owned(),
                        fix: vec![Edit {
                            start_byte: at,
                            end_byte: at + self.marker.len(),
                            replacement: String::new(),
                        }],
                        ..finding(1)
                    })
                    .collect();
                if !found.is_empty() {
                    reported.push((file.path.clone(), found));
                }
            }
            Ok(reported)
        }
    }

    /// Fixable lines start with `DROP` (the fix deletes the line), `a` becomes `b` and `b` becomes `c`
    /// (each a fix), and `KEEP_ME` is reported without one. Every finding carries the line it is on.
    struct Lines;

    impl Engine for Lines {
        fn name(&self) -> &'static str {
            "line-check"
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
            let mut reported = Vec::new();
            for file in req.files {
                let mut offset = 0;
                let mut found = Vec::new();
                for (index, line) in file.content.split_inclusive('\n').enumerate() {
                    let text = line.trim_end();
                    let fix = |replacement: &str, end: usize| {
                        vec![Edit {
                            start_byte: offset,
                            end_byte: offset + end,
                            replacement: replacement.to_owned(),
                        }]
                    };
                    let edits = match text {
                        _ if text.starts_with("DROP") => Some(fix("", line.len())),
                        "a" => Some(fix("b", 1)),
                        "b" => Some(fix("c", 1)),
                        "KEEP_ME" | "c" => Some(Vec::new()),
                        _ => None,
                    };
                    if let Some(fix) = edits {
                        found.push(Diagnostic {
                            engine: "line-check".to_owned(),
                            title: text.to_owned(),
                            fix,
                            ..finding(index as u32 + 1)
                        });
                    }
                    offset += line.len();
                }
                if !found.is_empty() {
                    reported.push((file.path.clone(), found));
                }
            }
            Ok(reported)
        }
    }

    fn fixing_run(config: &Config, options: RunOptions) -> LintRun {
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Marker {
            name: "dotenv",
            marker: "FLAG",
        })];
        with_extra_project_engines(engines, || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let dir = tempfile::tempdir().unwrap();
            let path = write(dir.path(), "app.py", "x = 1  # FLAG\n");
            let run = lint_run(&[dir.path().to_path_buf()], config, &options, true, false).unwrap();
            assert_eq!(
                fs::read_to_string(path).unwrap(),
                "x = 1  # \n",
                "the fix, hence a second pass, ran"
            );
            run
        })
    }

    #[test]
    fn an_unavailable_backend_error_is_reported_once_across_fix_passes() {
        let components = poly_config::v2::ComponentsConfig {
            disable: vec!["pyrefly".to_owned()],
            ..Default::default()
        };
        let run = fixing_run(&python_types_config("\"pyrefly\"", components), opts(true));
        let named = run
            .errors
            .iter()
            .filter(|e| e.message.contains("`pyrefly` component"))
            .count();
        assert_eq!(named, 1, "{:?}", run.errors);
    }

    #[test]
    fn a_counted_only_skip_is_reported_once_across_fix_passes() {
        let only = RunOptions {
            only: vec!["pyrefly".to_owned(), "dotenv".to_owned()],
            ..opts(true)
        };
        let run = fixing_run(&python_types_config("\"ty\"", Default::default()), only);
        assert!(run.errors.is_empty(), "{}", error_text(&run));
        let reason = "`--only pyrefly` selects nothing here: `[python-types] backend` is \"ty\"";
        let found = run.skipped.iter().filter(|s| s.reason == reason).count();
        assert_eq!(found, 1, "{:?}", run.skipped);
    }

    fn fix_run(engines: Vec<Arc<dyn Engine>>, file: &str, content: &str) -> (String, LintRun) {
        with_extra_project_engines(engines, || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let dir = tempfile::tempdir().unwrap();
            let path = write(dir.path(), file, content);
            let run = lint_run(
                &[dir.path().to_path_buf()],
                &Config::default(),
                &opts(true),
                true,
                false,
            )
            .unwrap();
            (fs::read_to_string(path).unwrap(), run)
        })
    }

    #[test]
    fn two_engines_flagging_one_file_both_get_their_fix_applied() {
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Marker {
                name: "marker-one",
                marker: "FLAG_ME",
            }),
            Arc::new(Marker {
                name: "marker-two",
                marker: "FLAG_TWO",
            }),
        ];
        let (after, run) = fix_run(engines, "a.py", "FLAG_ME = 1\nFLAG_TWO = 2\nFLAG_ME = 3\n");
        assert_eq!(after, " = 1\n = 2\n = 3\n");
        let fixed: usize = run.results.iter().map(|r| r.fixed).sum();
        assert_eq!(fixed, 3);
        let left: usize = run.results.iter().map(|r| r.diagnostics.len()).sum();
        assert_eq!(left, 0, "{:?}", run.results);
    }

    #[test]
    fn a_remaining_finding_is_reported_at_its_line_in_the_fixed_file() {
        let (after, run) = fix_run(vec![Arc::new(Lines)], "a.py", "DROP one\nDROP two\nKEEP_ME\n");
        assert_eq!(after, "KEEP_ME\n");
        let result = &run.results[0];
        assert_eq!(result.fixed, 2);
        let spans: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.engine == "line-check")
            .map(|d| (d.title.clone(), d.span.map(|s| s.start_line)))
            .collect();
        assert_eq!(
            spans,
            [("KEEP_ME".to_owned(), Some(1))],
            "it moved from line 3 to line 1"
        );
    }

    #[test]
    fn a_finding_the_first_pass_exposes_is_fixed_in_one_more_pass_and_no_further() {
        let (after, run) = fix_run(vec![Arc::new(Lines)], "a.py", "a\n");
        assert_eq!(after, "c\n", "a -> b in the first pass, b -> c in the one extra pass");
        let result = &run.results[0];
        assert_eq!(result.fixed, 2);
        // A third pass would report `c`, which `Lines` flags without a fix.
        let left: Vec<_> = result.diagnostics.iter().filter(|d| d.engine == "line-check").collect();
        assert!(left.is_empty(), "the extra pass is the last one: {left:?}");
    }

    /// Reports `WATCH` lines without a fix, counts its runs, and fails its first run when `fail_first` is set.
    struct Watcher {
        languages: &'static [Language],
        name: &'static str,
        calls: Arc<AtomicUsize>,
        fail_first: bool,
    }

    impl Engine for Watcher {
        fn name(&self) -> &'static str {
            self.name
        }
        fn languages(&self) -> &'static [Language] {
            self.languages
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
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 && self.fail_first {
                anyhow::bail!("{} could not run", self.name);
            }
            Ok(req
                .files
                .iter()
                .filter(|file| file.content.contains("WATCH"))
                .map(|file| {
                    let found = Diagnostic {
                        engine: self.name.to_owned(),
                        ..finding(1)
                    };
                    (file.path.clone(), vec![found])
                })
                .collect())
        }
    }

    fn two_file_run(no_cache: bool, watched: &str, watcher: Watcher) -> (LintRun, usize) {
        let calls = watcher.calls.clone();
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Marker {
                name: "marker-one",
                marker: "FLAG_ME",
            }),
            Arc::new(watcher),
        ];
        let run = with_extra_project_engines(engines, || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "a.py", "FLAG_ME = 1\n");
            write(dir.path(), watched, "WATCH = 1\n");
            lint_run(
                &[dir.path().to_path_buf()],
                &Config::default(),
                &opts(no_cache),
                true,
                false,
            )
            .unwrap()
        });
        (run, calls.load(Ordering::SeqCst))
    }

    fn watched_findings(run: &LintRun, engine: &str) -> usize {
        run.results
            .iter()
            .flat_map(|r| &r.diagnostics)
            .filter(|d| d.engine == engine)
            .count()
    }

    #[test]
    fn an_engine_that_fixed_nothing_and_saw_no_rewritten_file_is_not_run_again() {
        for no_cache in [true, false] {
            let watcher = Watcher {
                languages: &[Language::Markdown],
                name: "watcher",
                calls: Arc::new(AtomicUsize::new(0)),
                fail_first: false,
            };
            let (run, calls) = two_file_run(no_cache, "b.md", watcher);
            assert_eq!(calls, 1, "no_cache = {no_cache}: its input did not change");
            assert_eq!(
                watched_findings(&run, "watcher"),
                1,
                "its first-pass finding stands, once"
            );
            let fixed: usize = run.results.iter().map(|r| r.fixed).sum();
            assert_eq!(fixed, 1);
        }
    }

    #[test]
    fn an_engine_handed_a_rewritten_file_runs_again_without_a_finding_in_it() {
        for no_cache in [true, false] {
            let watcher = Watcher {
                languages: &[Language::Python],
                name: "watcher",
                calls: Arc::new(AtomicUsize::new(0)),
                fail_first: false,
            };
            // It reads a.py, which marker-one rewrites, and reports only on b.py.
            let (run, calls) = two_file_run(no_cache, "b.py", watcher);
            assert_eq!(calls, 2, "no_cache = {no_cache}: a.py changed under it");
            assert_eq!(
                watched_findings(&run, "watcher"),
                1,
                "reported once, from the second pass"
            );
        }
    }

    #[test]
    fn an_engine_with_a_finding_in_a_rewritten_file_runs_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let engines: Vec<Arc<dyn Engine>> = vec![
            Arc::new(Marker {
                name: "marker-one",
                marker: "FLAG_ME",
            }),
            Arc::new(Watcher {
                languages: &[Language::Python],
                name: "watcher",
                calls: calls.clone(),
                fail_first: false,
            }),
        ];
        let run = with_extra_project_engines(engines, || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "a.py", "FLAG_ME WATCH = 1\n");
            lint_run(
                &[dir.path().to_path_buf()],
                &Config::default(),
                &opts(true),
                true,
                false,
            )
            .unwrap()
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2, "its view of a.py changed");
        assert_eq!(
            watched_findings(&run, "watcher"),
            1,
            "reported once, from the second pass"
        );
    }

    #[test]
    fn a_first_pass_error_survives_the_second_pass() {
        let watcher = Watcher {
            languages: &[Language::Python],
            name: "flaky",
            calls: Arc::new(AtomicUsize::new(0)),
            fail_first: true,
        };
        let (run, _) = two_file_run(true, "b.py", watcher);
        let messages: Vec<&str> = run.errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("flaky could not run")),
            "the engine failed once and was not asked again: {messages:?}"
        );
        assert_eq!(messages.len(), 1, "{messages:?}");
    }

    #[test]
    fn a_run_without_fixes_to_apply_runs_the_check_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let engine: Arc<dyn Engine> = Arc::new(Synthetic { calls: calls.clone() });
        with_extra_project_engines(vec![engine], || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: serialised by `with_extra_project_engines`.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let dir = tempfile::tempdir().unwrap();
            write(dir.path(), "a.py", "x = 1\n");
            lint_run(
                &[dir.path().to_path_buf()],
                &Config::default(),
                &opts(true),
                true,
                false,
            )
            .unwrap();
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

mod whole_tree {
    //! An engine whose verdict is about the whole tree (a workspace graph) must see what `.gitignore`
    //! and the pruned directories leave, not what `[discovery] exclude`, `--exclude` or a nested
    //! `poly.toml` carve out; the run's files only decide which findings are reported.

    use super::*;
    use poly_core::engine::{ProjectDirs, ProjectInputs};

    const NAME: &str = "graph-check";

    /// Reports on every served file the sorted list of files (relative names) it was handed, so a test
    /// reads what the engine saw from the finding itself.
    struct Graph {
        name: &'static str,
        whole_tree: bool,
        file_names: Vec<String>,
        /// Text every finding's fix deletes from the file it is on, when the file holds it.
        fix_marker: Option<&'static str>,
    }

    impl Graph {
        fn new(whole_tree: bool) -> Self {
            Self {
                name: NAME,
                whole_tree,
                file_names: vec![".graphignore".to_owned()],
                fix_marker: None,
            }
        }
    }

    impl Engine for Graph {
        fn name(&self) -> &'static str {
            self.name
        }
        fn languages(&self) -> &'static [Language] {
            &[Language::Markdown]
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
        fn project_inputs(&self) -> ProjectInputs {
            ProjectInputs {
                gate_key: None,
                files: Vec::new(),
                dirs: Some(ProjectDirs {
                    dir_key: "dir".to_owned(),
                    dir_default: ".".to_owned(),
                    paths_key: None,
                    recursive_key: None,
                    recursive_default: true,
                    whole_tree: self.whole_tree,
                    file_names: self.file_names.clone(),
                    list_unserved: false,
                }),
            }
        }
        fn project_check(&self, req: &ProjectRequest<'_>) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
            let mut seen: Vec<String> = req
                .files
                .iter()
                .map(|f| f.path.strip_prefix(req.root).unwrap_or(&f.path).display().to_string())
                .collect();
            seen.sort();
            let title = seen.join(",");
            Ok(req
                .files
                .iter()
                .filter(|f| f.language == Language::Markdown)
                .map(|f| {
                    (
                        f.path.clone(),
                        vec![Diagnostic {
                            engine: self.name.to_owned(),
                            title: title.clone(),
                            fix: self
                                .fix_marker
                                .and_then(|marker| f.content.find(marker).map(|at| (at, marker.len())))
                                .map(|(at, len)| Edit {
                                    start_byte: at,
                                    end_byte: at + len,
                                    replacement: String::new(),
                                })
                                .into_iter()
                                .collect(),
                            ..finding(1)
                        }],
                    )
                })
                .collect())
        }
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "poly.toml",
            "[workspace]\nroot = true\n\n[discovery]\nexclude = [\"legacy/**\"]\n",
        );
        write(dir.path(), ".gitignore", "gen/\n");
        write(dir.path(), ".graphignore", "x\n");
        write(dir.path(), "app/a.md", "# a\n");
        write(dir.path(), "legacy/old.md", "# old\n");
        write(dir.path(), "legacy/.graphignore", "y\n");
        write(dir.path(), "gen/out.md", "# generated\n");
        write(dir.path(), "extra/e.md", "# extra\n");
        write(dir.path(), "nested/poly.toml", "[format]\n");
        write(dir.path(), "nested/n.md", "# nested\n");
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        dir
    }

    fn run_with(whole_tree: bool, exclude: &[&str]) -> (Vec<(String, String)>, tempfile::TempDir) {
        let dir = project();
        let engine: Arc<dyn Engine> = Arc::new(Graph::new(whole_tree));
        let found = with_extra_project_engines(vec![engine], || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let options = RunOptions {
                jobs: Some(1),
                exclude: exclude.iter().map(|glob| (*glob).to_owned()).collect(),
                ..RunOptions::default()
            };
            let run = lint_run(&[dir.path().to_path_buf()], &Config::default(), &options, false, false).unwrap();
            run.results
                .iter()
                .flat_map(|r| {
                    let name = r.path.strip_prefix(dir.path()).unwrap().display().to_string();
                    r.diagnostics
                        .iter()
                        .filter(|d| d.engine == NAME)
                        .map(move |d| (name.clone(), d.title.clone()))
                })
                .collect::<Vec<_>>()
        });
        (found, dir)
    }

    fn names(found: &[(String, String)]) -> Vec<&str> {
        let mut files: Vec<&str> = found.iter().map(|(file, _)| file.as_str()).collect();
        files.sort_unstable();
        files
    }

    #[test]
    fn excludes_and_nested_configs_do_not_cut_the_graph_and_gitignore_does() {
        let (found, _dir) = run_with(true, &["extra/**"]);
        let everything = ".graphignore,app/a.md,extra/e.md,legacy/.graphignore,legacy/old.md,nested/n.md";
        assert!(!found.is_empty());
        for (file, seen) in &found {
            assert_eq!(seen, everything, "what the engine saw when reporting on {file}");
        }
        assert_eq!(
            names(&found),
            ["app/a.md", "nested/n.md"],
            "an excluded file is seen but not reported on; a gitignored one is neither"
        );
    }

    #[test]
    fn a_tree_that_is_not_whole_keeps_honoring_excludes_and_the_config_split() {
        let (mut found, _dir) = run_with(false, &["extra/**"]);
        found.sort();
        let row = |file: &str, seen: &str| (file.to_owned(), seen.to_owned());
        assert_eq!(
            found,
            [row("app/a.md", ".graphignore,app/a.md"), row("nested/n.md", "n.md"),],
            "the root scope misses legacy/, extra/ and nested/; nested/ is a scope of its own"
        );
    }

    /// A project with a nested config, the engine on for the whole tree, and the nested `poly.toml` set to `nested`.
    fn nested_project(root: &str, nested: &str) -> tempfile::TempDir {
        let dir = project();
        write(
            dir.path(),
            "poly.toml",
            &format!("[workspace]\nroot = true\n\n[discovery]\nexclude = [\"legacy/**\"]\n\n{root}"),
        );
        write(dir.path(), "nested/poly.toml", nested);
        dir
    }

    /// Lint `paths` under `dir` with `engines` registered; `(file relative to dir, severity, code, title)` of
    /// every finding by the engines named in `names`.
    fn lint_findings(
        dir: &Path,
        paths: &[&str],
        engines: Vec<Arc<dyn Engine>>,
        names: &[&str],
    ) -> (Vec<(String, Severity, String, String)>, LintRun) {
        lint_findings_with(dir, paths, engines, names, RunOptions::default(), false)
    }

    /// [`lint_findings`] under `options` (its `jobs` and `no_cache` are set here) and, with `fix`, `--fix`.
    fn lint_findings_with(
        dir: &Path,
        paths: &[&str],
        engines: Vec<Arc<dyn Engine>>,
        names: &[&str],
        options: RunOptions,
        fix: bool,
    ) -> (Vec<(String, Severity, String, String)>, LintRun) {
        with_extra_project_engines(engines, || {
            let cache_home = tempfile::tempdir().unwrap();
            // SAFETY: `with_extra_project_engines` serialises every caller in this process.
            unsafe {
                std::env::set_var("POLY_CACHE_HOME", cache_home.path());
            }
            let options = RunOptions {
                jobs: Some(1),
                no_cache: true,
                ..options
            };
            let paths: Vec<PathBuf> = paths.iter().map(|path| dir.join(path)).collect();
            let run = lint_run(&paths, &Config::default(), &options, fix, false).unwrap();
            let mut found: Vec<_> = run
                .results
                .iter()
                .flat_map(|r| {
                    let name = r.path.strip_prefix(dir).unwrap_or(&r.path).display().to_string();
                    r.diagnostics
                        .iter()
                        .filter(|d| names.contains(&d.engine.as_str()))
                        .map(move |d| {
                            (
                                name.clone(),
                                d.severity,
                                d.code.clone().unwrap_or_default(),
                                d.title.clone(),
                            )
                        })
                })
                .collect();
            found.sort_by(|a, b| (&a.0, &a.2, &a.3).cmp(&(&b.0, &b.2, &b.3)));
            (found, run)
        })
    }

    fn graph() -> Vec<Arc<dyn Engine>> {
        vec![Arc::new(Graph::new(true))]
    }

    fn files_of(found: &[(String, Severity, String, String)]) -> Vec<&str> {
        found.iter().map(|(file, ..)| file.as_str()).collect()
    }

    #[test]
    fn a_nested_config_that_disables_the_engine_silences_its_own_subtree() {
        let dir = nested_project("", "[engines.graph-check]\nenabled = false\n");
        let (found, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        assert_eq!(
            files_of(&found),
            ["app/a.md", "extra/e.md"],
            "nested/n.md is governed by the config that turned it off"
        );
    }

    #[test]
    fn a_nested_per_file_ignore_and_rule_level_apply_to_the_nested_files() {
        let dir = nested_project("", "[per-file-ignores]\n\"n.md\" = [\"PRJ001\"]\n");
        let (found, run) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        assert_eq!(files_of(&found), ["app/a.md", "extra/e.md"]);
        assert!(run.suppressed.iter().any(|s| s.path.ends_with("nested/n.md")));

        let dir = nested_project("", "[engines.graph-check.rules.PRJ001]\nlevel = \"error\"\n");
        let (found, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        let severities: Vec<_> = found
            .iter()
            .map(|(file, severity, ..)| (file.as_str(), *severity))
            .collect();
        assert_eq!(
            severities,
            [
                ("app/a.md", Severity::Warning),
                ("extra/e.md", Severity::Warning),
                ("nested/n.md", Severity::Error)
            ]
        );
    }

    #[test]
    fn a_file_named_by_a_roundabout_path_is_still_governed_by_its_nested_config() {
        let dir = nested_project("", "[engines.graph-check]\nenabled = true\n");
        let (found, _) = lint_findings(dir.path(), &["app/../nested/n.md"], graph(), &[NAME]);
        assert_eq!(found.len(), 1, "the roundabout path is reported: {found:?}");

        let dir = nested_project("", "[engines.graph-check]\nenabled = false\n");
        let (found, _) = lint_findings(dir.path(), &["app/../nested/n.md"], graph(), &[NAME]);
        assert_eq!(files_of(&found), [] as [&str; 0], "{found:?}");

        let dir = nested_project("", "[per-file-ignores]\n\"n.md\" = [\"PRJ001\"]\n");
        let (found, run) = lint_findings(dir.path(), &["app/../nested/n.md"], graph(), &[NAME]);
        assert_eq!(files_of(&found), [] as [&str; 0], "{found:?}");
        assert!(run.suppressed.iter().any(|s| s.path.ends_with("n.md")));
    }

    #[test]
    fn a_nested_option_the_outer_plan_overrides_is_reported_on_the_nested_config() {
        let dir = nested_project("", "[engines.graph-check]\ndir = \"sub\"\n");
        let (found, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        let notice: Vec<_> = found
            .iter()
            .filter(|(_, _, code, _)| code == "nested-options-ignored")
            .collect();
        assert_eq!(notice.len(), 1, "{found:?}");
        assert_eq!(notice[0].0, "nested/poly.toml");
        assert_eq!(notice[0].1, Severity::Warning);

        let quiet = nested_project("", "[engines.graph-check]\nenabled = true\n");
        let (found, _) = lint_findings(quiet.path(), &["."], graph(), &[NAME]);
        assert!(
            found.iter().all(|(_, _, code, _)| code != "nested-options-ignored"),
            "{found:?}"
        );
    }

    #[test]
    fn a_run_on_a_nested_directory_judges_the_same_tree_as_a_run_on_the_project() {
        let dir = nested_project("", "[format]\n");
        let (whole, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        let (scoped, _) = lint_findings(dir.path(), &["nested"], graph(), &[NAME]);
        let nested_of_whole: Vec<_> = whole
            .iter()
            .filter(|(file, ..)| file.starts_with("nested/"))
            .cloned()
            .collect();
        assert_eq!(files_of(&scoped), ["nested/n.md"]);
        assert_eq!(scoped, nested_of_whole, "the fragment under nested/ is not the verdict");
    }

    #[test]
    fn a_nested_config_the_outer_directory_does_not_reach_is_analysed_on_its_own() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "poly.toml",
            "[workspace]\nroot = true\n\n[engines.graph-check]\ndir = \"frontend\"\n",
        );
        write(dir.path(), "frontend/f.md", "# f\n");
        write(dir.path(), "backend/poly.toml", "[engines.graph-check]\ndir = \".\"\n");
        write(dir.path(), "backend/b.md", "# b\n");
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let (found, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        let seen: Vec<_> = found
            .iter()
            .map(|(file, _, _, title)| (file.as_str(), title.as_str()))
            .collect();
        assert_eq!(
            seen,
            [("backend/b.md", "b.md"), ("frontend/f.md", "frontend/f.md")],
            "frontend is judged from the root, backend from its own config"
        );
    }

    #[test]
    fn a_file_name_an_engine_declared_is_not_handed_to_another_engine() {
        let dir = project();
        let plain = Graph {
            name: "plain-check",
            whole_tree: true,
            file_names: Vec::new(),
            fix_marker: None,
        };
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(Graph::new(true)), Arc::new(plain)];
        let (_, run) = lint_findings(dir.path(), &["."], engines, &[NAME, "plain-check"]);
        let seen = |engine: &str| -> Vec<String> {
            run.results
                .iter()
                .flat_map(|r| &r.diagnostics)
                .filter(|d| d.engine == engine)
                .map(|d| d.title.clone())
                .collect()
        };
        let (declaring, other) = (seen(NAME), seen("plain-check"));
        assert!(!declaring.is_empty() && !other.is_empty());
        assert!(declaring.iter().all(|t| t.contains(".graphignore")), "{declaring:?}");
        assert!(other.iter().all(|t| !t.contains(".graphignore")), "{other:?}");
    }

    #[test]
    fn a_nested_gitignore_prunes_its_own_subtree_of_the_graph() {
        let dir = project();
        write(dir.path(), "app/.gitignore", "skipped/\n");
        write(dir.path(), "app/skipped/x.md", "# skipped\n");
        write(dir.path(), "app/kept/y.md", "# kept\n");
        let (found, _) = lint_findings(dir.path(), &["."], graph(), &[NAME]);
        assert!(!found.is_empty());
        for (file, _, _, seen) in &found {
            assert!(!seen.contains("skipped"), "{file} saw an ignored file: {seen}");
            assert!(seen.contains("app/kept/y.md"), "{file}: {seen}");
        }
    }

    fn selecting(only: &[&str], skip: &[&str]) -> RunOptions {
        RunOptions {
            only: only.iter().map(|s| (*s).to_owned()).collect(),
            skip: skip.iter().map(|s| (*s).to_owned()).collect(),
            ..RunOptions::default()
        }
    }

    #[test]
    fn an_ancestor_plan_honors_only_and_skip() {
        // Only a recognised engine name can be selected; `ini` serves no Markdown file, so it is inert beside this one.
        const NAME: &str = "ini";
        let graph = || -> Vec<Arc<dyn Engine>> {
            vec![Arc::new(Graph {
                name: NAME,
                ..Graph::new(true)
            })]
        };
        let dir = nested_project("", "[format]\n");
        let (baseline, _) = lint_findings(dir.path(), &["nested"], graph(), &[NAME]);
        assert!(
            baseline[0].3.contains("app/a.md"),
            "the root config's tree is judged: {baseline:?}"
        );

        let (only, _) = lint_findings_with(
            dir.path(),
            &["nested"],
            graph(),
            &[NAME],
            selecting(&[NAME], &[]),
            false,
        );
        assert_eq!(only, baseline);

        let (skipped, _) = lint_findings_with(
            dir.path(),
            &["nested"],
            graph(),
            &[NAME],
            selecting(&[], &[NAME]),
            false,
        );
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    #[test]
    fn a_disabled_ancestor_config_does_not_run_the_engine_for_a_nested_run() {
        let enabled = nested_project("[engines.graph-check]\nenabled = true\n", "[format]\n");
        let (found, _) = lint_findings(enabled.path(), &["nested"], graph(), &[NAME]);
        assert!(
            found.iter().any(|(_, _, _, seen)| seen.contains("app/a.md")),
            "control: {found:?}"
        );

        let dir = nested_project("[engines.graph-check]\nenabled = false\n", "[format]\n");
        let (found, _) = lint_findings(dir.path(), &["nested"], graph(), &[NAME]);
        assert_eq!(found, [], "a disabled config judges no tree, its own or its parent's");
    }

    fn notices(found: &[(String, Severity, String, String)]) -> Vec<&(String, Severity, String, String)> {
        found
            .iter()
            .filter(|(_, _, code, _)| code == "nested-options-ignored")
            .collect()
    }

    #[test]
    fn a_path_scoped_run_reports_a_differing_nested_option_once() {
        let dir = nested_project("", "[engines.graph-check]\ndir = \"sub\"\n");
        let (found, _) = lint_findings(dir.path(), &["nested"], graph(), &[NAME]);
        let notice = notices(&found);
        assert_eq!(notice.len(), 1, "{found:?}");
        assert_eq!(notice[0].0, "nested/poly.toml");
    }

    #[test]
    fn a_second_fix_pass_does_not_repeat_the_nested_options_note() {
        let dir = nested_project("", "[engines.graph-check]\ndir = \"sub\"\n");
        write(dir.path(), "app/a.md", "# a FLAG\n");
        let engine = Graph {
            fix_marker: Some("FLAG"),
            ..Graph::new(true)
        };
        let engines: Vec<Arc<dyn Engine>> = vec![Arc::new(engine)];
        let (found, _) = lint_findings_with(dir.path(), &["."], engines, &[NAME], RunOptions::default(), true);
        assert_eq!(
            fs::read_to_string(dir.path().join("app/a.md")).unwrap(),
            "# a \n",
            "the fix ran, so a second pass did too"
        );
        assert_eq!(notices(&found).len(), 1, "{found:?}");
    }
}
