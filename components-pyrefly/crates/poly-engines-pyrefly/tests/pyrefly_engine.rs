//! Fixtures for the pyrefly-backed project check, the same expectation set as ty's
//! (`crates/poly-engines-python/tests/ty_engine.rs`) with pyrefly's own diagnostic codes.
//!
//! - `known_type_errors` — a file with real type errors asserts the exact diagnostics.
//! - `site_packages_resolution` — a fake venv layout with a stub package proves an import resolves
//!   through the host-resolved `site-packages`, and that the same project without the environment
//!   does not (the control that shows the first assertion can fail).
//! - `no_subprocess_is_spawned` — an interpreter that records being run is never run.
//! - the `backend` gate: the engine does nothing until `backend = "pyrefly"` is set.

use std::fs;
use std::path::{Path, PathBuf};

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{
    Diagnostic, Engine, ProjectEnvironment, ProjectFile, ProjectRequest, PythonEnvironment, Severity,
};
use poly_engine_api::language::Language;
use poly_engines_pyrefly::PyreflyEngine;

fn config(options: &[(&str, toml::Value)]) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: options.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
    }
}

fn pyrefly_config() -> EngineConfig {
    config(&[("backend", toml::Value::String("pyrefly".into()))])
}

/// Write `files` under `root` and return them as the host would hand them over.
fn project(root: &Path, files: &[(&str, &str)]) -> Vec<ProjectFile> {
    files
        .iter()
        .map(|(name, content)| {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            ProjectFile {
                path,
                language: Language::Python,
                content: (*content).into(),
            }
        })
        .collect()
}

fn run(
    root: &Path,
    files: &[ProjectFile],
    config: &EngineConfig,
    environment: &ProjectEnvironment,
) -> anyhow::Result<Vec<(PathBuf, Vec<Diagnostic>)>> {
    PyreflyEngine.project_check(&ProjectRequest {
        root,
        files,
        config,
        environment,
        threads: None,
    })
}

fn codes(reported: &[(PathBuf, Vec<Diagnostic>)]) -> Vec<&str> {
    reported
        .iter()
        .flat_map(|(_, ds)| ds.iter().map(|d| d.code.as_deref().unwrap()))
        .collect()
}

/// `file:line:col code message`, one per finding, in report order.
fn summarize(root: &Path, reported: &[(PathBuf, Vec<Diagnostic>)]) -> Vec<String> {
    reported
        .iter()
        .flat_map(|(path, diagnostics)| {
            diagnostics.iter().map(move |d| {
                let span = d.span.as_ref().expect("pyrefly findings carry a span");
                format!(
                    "{}:{}:{} {} {}",
                    path.strip_prefix(root).unwrap().display(),
                    span.start_line,
                    span.start_col,
                    d.code.as_deref().unwrap_or("-"),
                    d.title,
                )
            })
        })
        .collect()
}

const KNOWN_BAD: &str = "\
def add(a: int, b: int) -> int:
    return a + b


add(\"x\", 1)
count: int = \"three\"
";

#[test]
fn known_type_errors() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", KNOWN_BAD)]);
    let reported = run(dir.path(), &files, &pyrefly_config(), &ProjectEnvironment::default()).unwrap();

    assert_eq!(reported.len(), 1, "one file with findings: {reported:?}");
    assert_eq!(reported[0].0, dir.path().join("app.py"));
    let diagnostics = &reported[0].1;
    assert!(diagnostics.iter().all(|d| d.engine == "pyrefly"));
    assert!(diagnostics.iter().all(|d| d.severity == Severity::Error));
    let by_code: Vec<(&str, u32)> = diagnostics
        .iter()
        .map(|d| (d.code.as_deref().unwrap(), d.span.as_ref().unwrap().start_line))
        .collect();
    assert_eq!(by_code, [("bad-argument-type", 5), ("bad-assignment", 6)]);
    insta::assert_snapshot!(summarize(dir.path(), &reported).join("\n"));
}

#[test]
fn clean_project_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("ok.py", "x: int = 1\n")]);
    let reported = run(dir.path(), &files, &pyrefly_config(), &ProjectEnvironment::default()).unwrap();
    assert!(reported.is_empty(), "{reported:?}");
}

#[test]
fn findings_are_computed_on_the_handed_over_content() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = project(dir.path(), &[("app.py", "x: int = 1\n")]);
    files[0].content = "x: int = \"not on disk\"\n".into();
    let reported = run(dir.path(), &files, &pyrefly_config(), &ProjectEnvironment::default()).unwrap();
    assert_eq!(codes(&reported), ["bad-assignment"], "{reported:?}");
}

#[test]
fn files_import_one_another() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(
        dir.path(),
        &[
            ("pkg/__init__.py", ""),
            ("pkg/util.py", "def answer() -> int:\n    return 1\n"),
            ("app.py", "from pkg.util import answer\n\nvalue: str = answer()\n"),
        ],
    );
    let reported = run(dir.path(), &files, &pyrefly_config(), &ProjectEnvironment::default()).unwrap();
    assert_eq!(codes(&reported), ["bad-assignment"], "{reported:?}");
    assert_eq!(reported[0].0, dir.path().join("app.py"));
}

const APP_USING_STUB: &str = "\
from stubpkg import answer

value: str = answer()
";

/// A venv with `lib/python3.12/site-packages/stubpkg/__init__.pyi` and the environment the host
/// would resolve for it.
fn venv_with_stub(root: &Path) -> ProjectEnvironment {
    let site_packages = root.join(".venv/lib/python3.12/site-packages");
    let package = site_packages.join("stubpkg");
    fs::create_dir_all(&package).unwrap();
    fs::write(package.join("__init__.pyi"), "def answer() -> int: ...\n").unwrap();
    ProjectEnvironment {
        python: Some(PythonEnvironment {
            interpreter: root.join(".venv/bin/python"),
            site_packages: vec![site_packages],
            version: Some("3.12.0".into()),
        }),
    }
}

#[test]
fn site_packages_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let environment = venv_with_stub(dir.path());
    let files = project(dir.path(), &[("app.py", APP_USING_STUB)]);

    let resolved = run(dir.path(), &files, &pyrefly_config(), &environment).unwrap();
    assert_eq!(
        codes(&resolved),
        ["bad-assignment"],
        "the import resolved to `answer() -> int`, so assigning it to `str` is the only error: {resolved:?}"
    );

    let unresolved = run(dir.path(), &files, &pyrefly_config(), &ProjectEnvironment::default()).unwrap();
    assert!(
        codes(&unresolved).contains(&"missing-import"),
        "without the environment the import must not resolve: {unresolved:?}"
    );
}

/// The directory whose `python` and `python3` record that they were run, set for the worker below.
const GUARD_ENV: &str = "POLY_PYREFLY_NO_SUBPROCESS_DIR";

/// Fake interpreters on a `PATH` of their own. pyrefly runs one to learn the environment unless the
/// query is skipped, so a marker that never appears proves no process was spawned.
#[cfg(unix)]
#[test]
fn no_subprocess_is_spawned() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("interpreter-ran");
    for name in ["python", "python3"] {
        let script = dir.path().join(name);
        fs::write(&script, format!("#!/bin/sh\necho ran > '{}'\n", marker.display())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The control: with this PATH the interpreter is found, and it records.
    let found = std::process::Command::new("python3").env("PATH", dir.path()).status();
    assert!(found.unwrap().success());
    assert!(marker.exists(), "the recording interpreter must record");
    fs::remove_file(&marker).unwrap();

    let worker = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "no_subprocess_worker", "--test-threads=1"])
        .env(GUARD_ENV, dir.path())
        .env("PATH", dir.path())
        .output()
        .unwrap();
    assert!(
        worker.status.success(),
        "the worker failed: {}{}",
        String::from_utf8_lossy(&worker.stdout),
        String::from_utf8_lossy(&worker.stderr)
    );
    assert!(!marker.exists(), "an interpreter was executed");
}

/// Runs the check; the test above runs this in a child whose `PATH` holds only the recording interpreters.
#[test]
fn no_subprocess_worker() {
    let Some(guard) = std::env::var_os(GUARD_ENV) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", KNOWN_BAD)]);
    let environment = ProjectEnvironment {
        python: Some(PythonEnvironment {
            interpreter: Path::new(&guard).join("python"),
            site_packages: Vec::new(),
            version: Some("3.12.0".into()),
        }),
    };
    let reported = run(dir.path(), &files, &pyrefly_config(), &environment).unwrap();
    assert_eq!(
        codes(&reported),
        ["bad-argument-type", "bad-assignment"],
        "{reported:?}"
    );
}

#[test]
fn engine_is_off_without_the_backend_key() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", KNOWN_BAD)]);
    let reported = run(dir.path(), &files, &config(&[]), &ProjectEnvironment::default()).unwrap();
    assert!(reported.is_empty());
}

#[test]
fn engine_stands_down_for_the_ty_backend() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", KNOWN_BAD)]);
    let ty = config(&[("backend", toml::Value::String("ty".into()))]);
    assert!(
        run(dir.path(), &files, &ty, &ProjectEnvironment::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn python_version_option_drives_version_dependent_checks() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", "import tomllib\n")]);
    let with_version = |version: &str| {
        config(&[
            ("backend", toml::Value::String("pyrefly".into())),
            ("python_version", toml::Value::String(version.into())),
        ])
    };
    let old = run(
        dir.path(),
        &files,
        &with_version("3.10"),
        &ProjectEnvironment::default(),
    )
    .unwrap();
    assert_eq!(codes(&old), ["missing-import"], "tomllib is 3.11+: {old:?}");

    let new = run(
        dir.path(),
        &files,
        &with_version("3.12"),
        &ProjectEnvironment::default(),
    )
    .unwrap();
    assert!(new.is_empty(), "{new:?}");
}

#[test]
fn invalid_python_version_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", "x = 1\n")]);
    let cfg = config(&[
        ("backend", toml::Value::String("pyrefly".into())),
        ("python_version", toml::Value::String("three".into())),
    ]);
    let error = run(dir.path(), &files, &cfg, &ProjectEnvironment::default()).unwrap_err();
    assert!(error.to_string().contains("python_version"), "{error:#}");
}

#[test]
fn version_names_the_pinned_tag() {
    assert!(PyreflyEngine.version().contains("1.4.1"));
}
