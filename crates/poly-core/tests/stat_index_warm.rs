//! End-to-end proof that the stat-validated fast path is both fast (an
//! unchanged clean file is not even opened) and never stale (every input that
//! can change a verdict changes the plan key, the stat, or both).
//!
//! "Not even opened" is asserted from the runner's own accounting of how each
//! file was resolved (stat hit, content hit or miss), captured from its debug log.
//!
//! Files must age past the index's racy window before they are recorded, so each
//! scenario sleeps once after writing its fixtures. `POLY_CACHE_HOME` is
//! process-global, so every test holds `ENV_LOCK`.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use poly_core::{Config, RunOptions};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const SETTLE: Duration = Duration::from_millis(2_300);

struct Project {
    _cache_home: tempfile::TempDir,
    dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Project {
    fn new(ruff_select: &str) -> Self {
        let guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let cache_home = tempfile::tempdir().unwrap();
        // SAFETY: serialized by `ENV_LOCK`, which the returned value holds until
        // it is dropped together with the override's tempdir.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", cache_home.path());
        }
        let project = Self {
            _cache_home: cache_home,
            dir: tempfile::tempdir().unwrap(),
            _guard: guard,
        };
        project.set_ruff_select(ruff_select);
        project
    }

    fn set_ruff_select(&self, select: &str) {
        self.write("poly.toml", &format!("[lint.python.ruff]\nselect = [\"{select}\"]\n"));
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn write(&self, name: &str, content: &str) {
        let path = self.path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn opts() -> RunOptions {
        RunOptions {
            jobs: Some(2),
            explicit_config: true,
            ..RunOptions::default()
        }
    }

    fn config(&self) -> Config {
        Config::load_file(&self.path("poly.toml")).unwrap()
    }

    fn lint(&self) -> LintOutcome {
        let (run, log) = capture(|| {
            poly_core::lint_run(
                &[self.dir.path().to_path_buf()],
                &self.config(),
                &Self::opts(),
                false,
                false,
            )
            .unwrap()
        });
        let mut findings: Vec<String> = run
            .results
            .iter()
            .flat_map(|result| {
                let name = relative(self.dir.path(), &result.path);
                result
                    .diagnostics
                    .iter()
                    .map(move |d| format!("{name}:{}", d.code.clone().unwrap_or_default()))
            })
            .collect();
        findings.sort();
        LintOutcome {
            findings,
            errors: run.errors.iter().map(|e| e.message.clone()).collect(),
            resolution: Resolution::parse(&log),
        }
    }

    fn fmt_changed(&self) -> FmtOutcome {
        let (run, log) = capture(|| {
            poly_core::format_run(
                &[self.dir.path().to_path_buf()],
                &self.config(),
                &Self::opts(),
                false,
                false,
            )
            .unwrap()
        });
        let mut changed: Vec<String> = run
            .results
            .iter()
            .filter(|r| r.changed)
            .map(|r| relative(self.dir.path(), &r.path))
            .collect();
        changed.sort();
        FmtOutcome {
            changed,
            errors: run.errors.iter().map(|e| e.message.clone()).collect(),
            resolution: Resolution::parse(&log),
        }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        // SAFETY: still under `ENV_LOCK` (the guard field drops after this).
        unsafe {
            std::env::remove_var("POLY_CACHE_HOME");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LintOutcome {
    findings: Vec<String>,
    errors: Vec<String>,
    resolution: Resolution,
}

struct FmtOutcome {
    changed: Vec<String>,
    errors: Vec<String>,
    resolution: Resolution,
}

/// How many files the fast path answered, and how.
#[derive(Debug, PartialEq, Eq)]
struct Resolution {
    stat_hits: usize,
    content_hits: usize,
    misses: usize,
}

impl Resolution {
    fn parse(log: &str) -> Self {
        let line = log
            .lines()
            .find(|line| line.contains("stat index resolution"))
            .unwrap_or_else(|| panic!("the run logged no resolution summary:\n{log}"));
        let field = |name: &str| -> usize {
            let at = line.find(&format!("{name}=")).unwrap() + name.len() + 1;
            line[at..]
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        Self {
            stat_hits: field("stat_hits"),
            content_hits: field("content_hits"),
            misses: field("misses"),
        }
    }
}

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture<T>(run: impl FnOnce() -> T) -> (T, String) {
    let buf = SharedBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let value = tracing::subscriber::with_default(subscriber, run);
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (value, log)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned()
}

fn settle() {
    std::thread::sleep(SETTLE);
}

#[test]
fn a_warm_run_does_not_open_unchanged_clean_files() {
    let project = Project::new("INP001");
    project.write("pkg/__init__.py", "");
    project.write("pkg/ok.py", "x = 1\n");
    settle();
    let cold = project.lint();
    assert_eq!(cold.resolution.stat_hits, 0);
    project.lint();

    let warm = project.lint();

    assert!(warm.findings.is_empty());
    assert!(warm.errors.is_empty());
    assert_eq!(
        warm.resolution,
        Resolution {
            stat_hits: 3,
            content_hits: 0,
            misses: 0
        },
        "every file, poly.toml included, must be answered from its stat alone"
    );
}

#[test]
fn a_file_with_findings_is_replayed_without_running_an_engine() {
    let project = Project::new("INP001");
    project.write("loose/mod.py", "x = 1\n");
    settle();
    let cold = project.lint();
    project.lint();
    let warm = project.lint();

    assert_eq!(cold.findings, vec!["loose/mod.py:INP001".to_owned()]);
    assert_eq!(warm.findings, cold.findings);
    assert_eq!(warm.resolution.stat_hits, 2, "mod.py and poly.toml");
}

#[test]
fn adding_and_removing_a_sibling_init_file_changes_the_findings_of_an_untouched_file() {
    let project = Project::new("INP001");
    project.write("pkg/mod.py", "x = 1\n");
    settle();
    project.lint();
    let warm = project.lint();
    assert_eq!(warm.findings, vec!["pkg/mod.py:INP001".to_owned()]);
    assert_eq!(
        warm.resolution.stat_hits, 2,
        "the premise: mod.py is served from the index"
    );

    project.write("pkg/__init__.py", "");
    assert!(
        project.lint().findings.is_empty(),
        "mod.py is byte-identical and its stat is unchanged, but its package root moved"
    );
    settle();
    project.lint();
    assert!(project.lint().findings.is_empty());

    fs::remove_file(project.path("pkg/__init__.py")).unwrap();
    assert_eq!(project.lint().findings, vec!["pkg/mod.py:INP001".to_owned()]);
}

#[test]
fn editing_the_config_invalidates_a_clean_verdict() {
    let project = Project::new("F401");
    project.write("loose/mod.py", "x = 1\n");
    settle();
    project.lint();
    let warm = project.lint();
    assert!(warm.findings.is_empty());
    assert_eq!(
        warm.resolution.stat_hits, 2,
        "the premise: the verdict is served from the index"
    );

    project.set_ruff_select("INP001");

    assert_eq!(project.lint().findings, vec!["loose/mod.py:INP001".to_owned()]);
}

#[test]
fn a_same_size_edit_that_keeps_the_mtime_is_still_detected() {
    let project = Project::new("F401");
    project.write("pkg/__init__.py", "");
    project.write("pkg/mod.py", "import_os\n");
    settle();
    project.lint();
    let warm = project.lint();
    assert!(warm.findings.is_empty());
    assert_eq!(
        warm.resolution.stat_hits, 3,
        "the premise: both files are served from the index"
    );

    let file = project.path("pkg/mod.py");
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, "import os\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();

    assert_eq!(project.lint().findings, vec!["pkg/mod.py:F401".to_owned()]);
}

#[test]
fn a_file_edited_inside_the_racy_window_is_never_trusted() {
    let project = Project::new("F401");
    project.write("pkg/__init__.py", "");
    project.write("pkg/mod.py", "import_os\n");
    settle();
    project.lint();

    project.write("pkg/mod.py", "import os\n");
    project.lint();
    project.write("pkg/mod.py", "import_os\n");
    project.lint();
    project.write("pkg/mod.py", "import os\n");

    assert_eq!(
        project.lint().findings,
        vec!["pkg/mod.py:F401".to_owned()],
        "back-to-back rewrites of one size within the timestamp granularity must never serve a stale clean verdict"
    );
}

#[test]
fn touching_a_file_without_changing_it_keeps_the_verdict_without_re_linting() {
    let project = Project::new("INP001");
    project.write("pkg/__init__.py", "");
    project.write("pkg/ok.py", "x = 1\n");
    project.write("loose/mod.py", "x = 1\n");
    settle();
    project.lint();
    let before = project.lint();

    project.write("pkg/ok.py", "x = 1\n");
    project.write("loose/mod.py", "x = 1\n");
    settle();
    let touched = project.lint();

    assert_eq!(touched.findings, before.findings);
    assert_eq!(
        touched.resolution.content_hits, 2,
        "identical bytes must be vouched for by digest"
    );
    assert_eq!(
        project.lint().resolution.stat_hits,
        4,
        "and the refreshed stat serves the next run"
    );
}

#[test]
fn a_warm_format_check_does_not_open_unchanged_files_and_still_reports_unformatted_ones() {
    let project = Project::new("F401");
    project.write("a.toml", "a = 1\n");
    project.write("b.toml", "b   =   2\n");
    settle();
    project.fmt_changed();
    let warm = project.fmt_changed();

    assert_eq!(warm.changed, vec!["b.toml".to_owned()]);
    assert!(warm.errors.is_empty());
    assert_eq!(
        warm.resolution.stat_hits, 2,
        "a.toml is answered from its stat; b.toml needs a formatter"
    );
}

#[test]
fn editing_a_formatted_file_into_an_unformatted_one_is_reported() {
    let project = Project::new("F401");
    project.write("a.toml", "a = 1\n");
    settle();
    project.fmt_changed();
    assert!(project.fmt_changed().changed.is_empty());

    project.write("a.toml", "a    = 1\n");

    assert_eq!(project.fmt_changed().changed, vec!["a.toml".to_owned()]);
}

/// The `prefilled` count of the run's lint batch, from its debug log.
fn batched(log: &str) -> usize {
    let line = log
        .lines()
        .find(|line| line.contains("lint batch prefill complete"))
        .unwrap_or_else(|| panic!("the run logged no batch summary:\n{log}"));
    let at = line.find("prefilled=").unwrap() + "prefilled=".len();
    line[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn batched_lint_results_are_recorded_and_a_warm_file_is_never_batched_again() {
    let project = Project::new("INP001");
    let names = ["a.sh", "b.sh", "c.sh", "d.sh"];
    for name in names {
        project.write(name, "#!/bin/sh\necho $1\n");
    }
    settle();
    let run = |project: &Project| {
        capture(|| {
            poly_core::lint_run(
                &[project.dir.path().to_path_buf()],
                &project.config(),
                &Project::opts(),
                false,
                false,
            )
            .unwrap()
        })
    };
    let findings = |run: &poly_core::LintRun| {
        let mut found: Vec<String> = run
            .results
            .iter()
            .filter(|result| result.path.extension().is_some_and(|ext| ext == "sh"))
            .flat_map(|result| result.diagnostics.iter().map(|d| d.code.clone().unwrap_or_default()))
            .collect();
        found.sort();
        found
    };

    let (cold, cold_log) = run(&project);
    if batched(&cold_log) == 0 {
        eprintln!("shellcheck is unavailable; nothing batched, so nothing to compose");
        return;
    }
    let expected = findings(&cold);
    assert!(!expected.is_empty(), "the premise: shellcheck reports on the fixtures");
    let (_, second_log) = run(&project);
    assert_eq!(batched(&second_log), 0, "every file is clean to the index by now or reused");

    let (warm, warm_log) = run(&project);
    assert_eq!(findings(&warm), expected, "batched findings must be replayed from the index");
    assert_eq!(batched(&warm_log), 0, "an indexed file must not be batched again");
    let resolution = Resolution::parse(&warm_log);
    assert_eq!(resolution.misses, 0, "{resolution:?}");

    project.write("a.sh", "#!/bin/sh\necho $2\n");
    let (changed, changed_log) = run(&project);
    assert_eq!(
        batched(&changed_log),
        0,
        "one changed file is below the batch size and the rest are indexed"
    );
    assert_eq!(findings(&changed).len(), expected.len());
}
