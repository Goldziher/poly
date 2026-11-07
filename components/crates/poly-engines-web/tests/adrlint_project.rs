//! The adrlint tree-wide rules (`MADR41a`/`b`, `MADR42`–`MADR44`) and the immutability lock
//! (`ADR50`/`ADR51`) as a project check: one fixture ADR log per rule, driven through the engine,
//! then through the pipeline to prove the cached result is invalidated when a sibling ADR or the
//! lock changes.
#![allow(unsafe_code, reason = "POLY_CACHE_HOME is set once, before any thread reads it")]

use std::fs;
use std::path::{Path, PathBuf};

use adrlint::{adr, lock};
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, ProjectEnvironment, ProjectFile, ProjectRequest};
use poly_engine_api::language::Language;
use poly_engine_api::project::LISTING_LANGUAGE;
use poly_engines_web::adrlint::AdrlintEngine;

mod common;

fn fixture(rule: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/adrlint/project")
        .join(rule)
}

fn cfg(extra: &[(&str, toml::Value)]) -> EngineConfig {
    let mut options = toml::Table::new();
    options.insert("enabled".to_string(), toml::Value::Boolean(true));
    for (key, value) in extra {
        options.insert((*key).to_string(), value.clone());
    }
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 2,
        options,
    }
}

/// Every file under `root`, the way the host hands them over: Markdown as Markdown with its
/// content, anything else as a name-only `Other("listing")` entry. The lock manifest arrives
/// separately as `Other("adrlint.lock")`, see [`lock_file`].
fn load(root: &Path) -> Vec<ProjectFile> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
            let (language, content) = if name.ends_with(".md") {
                (Language::Markdown, fs::read_to_string(&path).expect("read"))
            } else {
                (Language::Other(LISTING_LANGUAGE.to_owned()), String::new())
            };
            files.push(ProjectFile {
                content: content.into(),
                path,
                language,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// A lock manifest recording each `(relative path, number)` with the hash of `content`.
fn lock_text(entries: &[(&str, u32, &str)]) -> String {
    let mut manifest = lock::LockFile::default();
    for (path, number, content) in entries {
        manifest.insert(*path, lock::entry(&adr::parse(content), *number));
    }
    lock::render(&manifest).expect("render lock")
}

fn lock_file(root: &Path, text: String) -> ProjectFile {
    ProjectFile {
        path: root.join("adrlint.lock"),
        language: Language::Other("adrlint.lock".to_owned()),
        content: text.into(),
    }
}

/// `(file name, code)` of every finding.
fn check(root: &Path, files: &[ProjectFile], config: &EngineConfig) -> Vec<(String, String)> {
    let environment = ProjectEnvironment::default();
    let request = ProjectRequest {
        root,
        files,
        config,
        environment: &environment,
    };
    let mut found: Vec<(String, String)> = AdrlintEngine
        .project_check(&request)
        .expect("project check")
        .into_iter()
        .flat_map(|(path, diagnostics)| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
            diagnostics
                .into_iter()
                .map(move |d| (name.clone(), d.code.unwrap_or_default()))
        })
        .collect();
    found.sort();
    found
}

fn pair(name: &str, code: &str) -> (String, String) {
    (name.to_owned(), code.to_owned())
}

#[test]
fn a_clean_log_has_no_findings() {
    let root = fixture("clean");
    assert_eq!(check(&root, &load(&root), &cfg(&[])), []);
}

#[test]
fn madr41a_reports_a_log_that_does_not_start_at_zero_or_one() {
    let root = fixture("madr41a");
    assert_eq!(
        check(&root, &load(&root), &cfg(&[])),
        [pair("0005-use-rust.md", "MADR41a")]
    );
}

#[test]
fn madr41a_honours_a_configured_start() {
    let root = fixture("clean");
    let found = check(&root, &load(&root), &cfg(&[("start", toml::Value::Integer(0))]));
    assert_eq!(found, [pair("0001-use-rust.md", "MADR41a")]);
}

#[test]
fn madr41b_reports_a_gap_in_the_numbering() {
    let root = fixture("madr41b");
    assert_eq!(
        check(&root, &load(&root), &cfg(&[])),
        [pair("0003-use-go.md", "MADR41b")]
    );
}

#[test]
fn madr41b_is_silent_when_gaps_are_allowed() {
    let root = fixture("madr41b");
    let found = check(&root, &load(&root), &cfg(&[("allow_gaps", toml::Value::Boolean(true))]));
    assert_eq!(found, []);
}

#[test]
fn madr42_reports_a_non_adr_file_in_the_log() {
    let root = fixture("madr42");
    assert_eq!(check(&root, &load(&root), &cfg(&[])), [pair("notes.txt", "MADR42")]);
}

#[test]
fn madr43_reports_both_files_sharing_a_number() {
    let root = fixture("madr43");
    assert_eq!(
        check(&root, &load(&root), &cfg(&[])),
        [pair("0001-use-go.md", "MADR43"), pair("0001-use-rust.md", "MADR43")]
    );
}

#[test]
fn madr44_reports_a_malformed_file_name() {
    let root = fixture("madr44");
    assert_eq!(
        check(&root, &load(&root), &cfg(&[])),
        [pair("0002_use_go.md", "MADR44")]
    );
}

#[test]
fn adr50_reports_an_accepted_record_changed_after_locking() {
    let root = fixture("adr50");
    let mut files = load(&root);
    let original = fs::read_to_string(root.join("docs/adr/0001-use-rust.md")).expect("read");
    let locked = original.replace("Chosen option: Rust", "Chosen option: Zig");
    files.push(lock_file(
        &root,
        lock_text(&[("docs/adr/0001-use-rust.md", 1, &locked)]),
    ));
    assert_eq!(check(&root, &files, &cfg(&[])), [pair("0001-use-rust.md", "ADR50")]);
}

#[test]
fn adr50_is_silent_when_the_record_matches_its_lock() {
    let root = fixture("adr50");
    let mut files = load(&root);
    let original = fs::read_to_string(root.join("docs/adr/0001-use-rust.md")).expect("read");
    files.push(lock_file(
        &root,
        lock_text(&[("docs/adr/0001-use-rust.md", 1, &original)]),
    ));
    assert_eq!(check(&root, &files, &cfg(&[])), []);
}

#[test]
fn adr51_reports_a_locked_record_that_is_gone() {
    let root = fixture("adr51");
    let mut files = load(&root);
    let original = fs::read_to_string(root.join("docs/adr/0001-use-rust.md")).expect("read");
    files.push(lock_file(
        &root,
        lock_text(&[
            ("docs/adr/0001-use-rust.md", 1, &original),
            ("docs/adr/0002-use-go.md", 2, &original),
        ]),
    ));
    assert_eq!(check(&root, &files, &cfg(&[])), [pair("0002-use-go.md", "ADR51")]);
}

#[test]
fn a_selection_drops_unselected_rules() {
    let root = fixture("madr43");
    let ignore = toml::Value::Array(vec!["MADR43".into()]);
    assert_eq!(check(&root, &load(&root), &cfg(&[("ignore", ignore)])), []);
}

#[test]
fn a_listing_entry_is_never_mistaken_for_the_lock() {
    let root = fixture("madr42");
    let files = load(&root);
    assert!(
        files
            .iter()
            .any(|f| f.language == Language::Other(LISTING_LANGUAGE.to_owned())),
        "the fixture has a stray file"
    );
    assert_eq!(check(&root, &files, &cfg(&[])), [pair("notes.txt", "MADR42")]);
}

#[test]
fn a_lock_named_by_the_option_is_the_manifest_whatever_its_name() {
    let root = fixture("adr50");
    let mut files = load(&root);
    let original = fs::read_to_string(root.join("docs/adr/0001-use-rust.md")).expect("read");
    let locked = original.replace("Chosen option: Rust", "Chosen option: Zig");
    files.push(ProjectFile {
        path: root.join("locks/mine.lock"),
        language: Language::Other("mine.lock".to_owned()),
        content: lock_text(&[("docs/adr/0001-use-rust.md", 1, &locked)]).into(),
    });
    assert_eq!(check(&root, &files, &cfg(&[])), [pair("0001-use-rust.md", "ADR50")]);
}

#[test]
fn a_malformed_lock_is_an_error_not_silence() {
    let root = fixture("clean");
    let mut files = load(&root);
    files.push(lock_file(&root, "this is = not [valid".to_owned()));
    let environment = ProjectEnvironment::default();
    let config = cfg(&[]);
    let request = ProjectRequest {
        root: &root,
        files: &files,
        config: &config,
        environment: &environment,
    };
    assert!(AdrlintEngine.project_check(&request).is_err());
}

/// Drives `poly_core::lint_run` with the cache **on** and returns the project-check codes.
fn run(root: &Path) -> Vec<(String, String)> {
    common::register();
    let config = poly_core::Config::default();
    let options = poly_core::RunOptions {
        jobs: Some(1),
        ..poly_core::RunOptions::default()
    };
    let run = poly_core::lint_run(&[root.to_path_buf()], &config, &options, false, false).expect("lint run");
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    let mut found: Vec<(String, String)> = run
        .results
        .iter()
        .flat_map(|result| {
            let name = result
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            result
                .diagnostics
                .iter()
                .filter(|d| d.engine == "adrlint")
                .map(move |d| (name.clone(), d.code.clone().unwrap_or_default()))
        })
        .collect();
    found.sort();
    found
}

/// A project directory with an ADR log and a `poly.toml` enabling adrlint, under one cache directory
/// shared by every test in this binary (the variable is set once, before anything reads it).
fn project() -> (tempfile::TempDir, PathBuf, String) {
    static CACHE_HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    CACHE_HOME.get_or_init(|| {
        let dir = tempfile::tempdir().expect("cache dir");
        // SAFETY: set once, from the `OnceLock` initialiser, before any run in this binary reads it.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", dir.path());
        }
        dir
    });
    let project = tempfile::tempdir().expect("project");
    let adrs = project.path().join("docs/adr");
    fs::create_dir_all(&adrs).expect("mkdir");
    fs::write(
        project.path().join("poly.toml"),
        "[workspace]\nroot = true\n\n[engines.adrlint]\nenabled = true\nselect = [\"MADR41\", \"MADR42\", \"MADR43\", \"MADR44\", \"ADR5\"]\n",
    )
    .expect("write config");
    let template = fs::read_to_string(fixture("clean").join("docs/adr/0001-use-rust.md")).expect("template");
    fs::write(adrs.join("0001-use-rust.md"), &template).expect("write");
    (project, adrs, template)
}

/// The project-check result is cached on every file the check saw, so a change to a sibling ADR or
/// to the lock must re-run it: a stale entry would keep reporting the old verdict.
#[test]
fn the_cached_result_is_invalidated_by_a_sibling_adr_and_by_the_lock() {
    let (project, adrs, template) = project();

    assert_eq!(run(project.path()), [], "a single ADR is clean");
    assert_eq!(run(project.path()), [], "and stays clean from the cache");

    fs::write(adrs.join("0001-use-go.md"), &template).expect("write sibling");
    assert_eq!(
        run(project.path()),
        [pair("0001-use-go.md", "MADR43"), pair("0001-use-rust.md", "MADR43")],
        "a new sibling ADR re-runs the check"
    );

    fs::remove_file(adrs.join("0001-use-go.md")).expect("remove sibling");
    assert_eq!(run(project.path()), [], "removing it re-runs the check again");

    let locked = lock_text(&[("docs/adr/0001-use-rust.md", 1, &template)]);
    fs::write(project.path().join("adrlint.lock"), &locked).expect("write lock");
    assert_eq!(run(project.path()), [], "a lock that matches changes nothing");

    let stale = lock_text(&[(
        "docs/adr/0001-use-rust.md",
        1,
        &template.replace("Rust, because", "Zig, because"),
    )]);
    fs::write(project.path().join("adrlint.lock"), stale).expect("write stale lock");
    assert_eq!(
        run(project.path()),
        [pair("0001-use-rust.md", "ADR50")],
        "a changed lock re-runs the check"
    );

    fs::write(
        adrs.join("0001-use-rust.md"),
        template.replace("Rust, because", "Zig, because"),
    )
    .expect("edit");
    assert_eq!(
        run(project.path()),
        [],
        "editing the ADR back to its locked content clears ADR50"
    );
}

/// `MADR42` needs the names of the files that are not Markdown, which the host lists (names only)
/// for the declared ADR directory; adding or removing one must re-run the check.
#[test]
fn a_stray_file_in_the_log_is_reported_as_madr42_and_the_listing_invalidates_the_cache() {
    let (project, adrs, _) = project();
    assert_eq!(run(project.path()), []);

    fs::write(adrs.join("notes.txt"), "scratch\n").expect("write stray file");
    assert_eq!(run(project.path()), [pair("notes.txt", "MADR42")]);

    fs::remove_file(adrs.join("notes.txt")).expect("remove");
    assert_eq!(run(project.path()), []);
}

/// A log elsewhere is the one judged: the host hands over only the declared directory, so a log
/// with a gap under `docs/adr` is not reported when `dir` names another directory.
#[test]
fn a_directory_other_than_the_default_is_the_one_judged() {
    let (project, _, template) = project();
    fs::write(
        project.path().join("poly.toml"),
        "[workspace]\nroot = true\n\n[engines.adrlint]\nenabled = true\ndir = \"records\"\nselect = [\"MADR4\"]\n",
    )
    .expect("write config");
    fs::write(project.path().join("docs/adr/0009-gap.md"), &template).expect("gap outside");
    let records = project.path().join("records");
    fs::create_dir_all(&records).expect("mkdir");
    fs::write(records.join("0001-one.md"), &template).expect("write");
    fs::write(records.join("0003-three.md"), &template).expect("write");
    assert_eq!(
        run(project.path()),
        [pair("0003-three.md", "MADR41b")],
        "only the configured directory is judged"
    );
}

/// Naming one record must not judge the log as if it held that record alone: a log of two records
/// has no gap, but handed only the second the check would report one.
#[test]
fn naming_one_record_still_judges_the_whole_log() {
    let (project, adrs, template) = project();
    fs::write(adrs.join("0002-use-go.md"), &template).expect("write");
    // A positive control in the same run: the lock disagrees with the named record, so the check
    // demonstrably ran over it.
    let stale = lock_text(&[(
        "docs/adr/0002-use-go.md",
        2,
        &template.replace("Rust, because", "Zig, because"),
    )]);
    fs::write(project.path().join("adrlint.lock"), stale).expect("write lock");
    common::register();
    let options = poly_core::RunOptions {
        jobs: Some(1),
        ..poly_core::RunOptions::default()
    };
    let config = poly_core::Config::default();
    let one = adrs.join("0002-use-go.md");
    let run = poly_core::lint_run(&[one], &config, &options, false, false).expect("lint run");
    let codes: Vec<_> = run
        .results
        .iter()
        .flat_map(|r| &r.diagnostics)
        .filter(|d| d.engine == "adrlint")
        .map(|d| d.code.clone().unwrap_or_default())
        .collect();
    assert_eq!(
        codes,
        ["ADR50"],
        "the check ran, and found no false gap from a one-file run"
    );
}

/// Files outside the declared ADR directory are neither judged nor listed.
#[test]
fn files_outside_the_declared_directory_are_not_listed() {
    let (project, _, _) = project();
    fs::create_dir_all(project.path().join("elsewhere")).expect("mkdir");
    fs::write(project.path().join("elsewhere/notes.txt"), "x\n").expect("write");
    fs::write(project.path().join("notes.txt"), "x\n").expect("write");
    fs::write(project.path().join("docs/adr/stray.txt"), "x\n").expect("write");
    assert_eq!(
        run(project.path()),
        [pair("stray.txt", "MADR42")],
        "the check ran: the stray file inside the directory is the only one listed"
    );
}

#[test]
fn a_readme_in_the_adr_directory_is_not_a_finding() {
    let (project, adrs, template) = project();
    fs::write(adrs.join("README.md"), "# Decisions\n\nThe index.\n").expect("write readme");
    fs::write(adrs.join("0003-gap.md"), &template).expect("write gap");
    assert_eq!(
        run(project.path()),
        [pair("0003-gap.md", "MADR41b")],
        "the check ran, and the README is neither numbered nor stray"
    );
}

#[test]
fn the_project_phase_does_not_run_unless_adrlint_is_enabled() {
    let (project, adrs, template) = project();
    fs::write(adrs.join("0003-gap.md"), &template).expect("write gap");
    assert_eq!(
        run(project.path()),
        [pair("0003-gap.md", "MADR41b")],
        "enabled: reported"
    );
    fs::write(
        project.path().join("poly.toml"),
        "[workspace]\nroot = true\n\n[engines.adrlint]\nselect = [\"MADR4\"]\n",
    )
    .expect("write config");
    assert_eq!(run(project.path()), [], "not enabled: the same tree is not judged");
}

#[test]
fn recursive_takes_the_subdirectories_of_the_log() {
    let (project, adrs, template) = project();
    fs::create_dir_all(adrs.join("sub")).expect("mkdir");
    fs::write(adrs.join("sub/0003-gap.md"), &template).expect("write nested");
    assert_eq!(run(project.path()), [], "a nested record is outside a flat log");
    fs::write(
        project.path().join("poly.toml"),
        "[workspace]\nroot = true\n\n[engines.adrlint]\nenabled = true\nrecursive = true\nselect = [\"MADR41\"]\n",
    )
    .expect("write config");
    assert_eq!(
        run(project.path()),
        [pair("0003-gap.md", "MADR41a")],
        "recursive: the nested record is part of the log"
    );
}

/// A lock that exists but cannot be used must fail the run, not switch `ADR50`/`ADR51` off.
#[cfg(unix)]
#[test]
fn a_symlinked_lock_and_an_escaping_directory_are_run_errors() {
    let (project, _, _) = project();
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("real.lock"), "").expect("write");
    std::os::unix::fs::symlink(outside.path().join("real.lock"), project.path().join("adrlint.lock")).expect("symlink");
    common::register();
    let lint = |root: &Path| {
        poly_core::lint_run(
            &[root.to_path_buf()],
            &poly_core::Config::default(),
            &poly_core::RunOptions {
                jobs: Some(1),
                ..poly_core::RunOptions::default()
            },
            false,
            false,
        )
        .expect("lint run")
    };
    let run = lint(project.path());
    assert!(
        run.errors.iter().any(|e| e.message.contains("adrlint.lock")),
        "{:?}",
        run.errors
    );

    fs::remove_file(project.path().join("adrlint.lock")).expect("remove");
    fs::write(
        project.path().join("poly.toml"),
        "[workspace]\nroot = true\n\n[engines.adrlint]\nenabled = true\ndir = \"../adr\"\n",
    )
    .expect("write config");
    let run = lint(project.path());
    assert!(
        run.errors.iter().any(|e| e.message.contains("inside the project root")),
        "{:?}",
        run.errors
    );
}
