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
