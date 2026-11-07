//! Fixtures for the ty-backed `python-types` project check.
//!
//! - `known_type_errors` — a file with real type errors asserts the exact diagnostics.
//! - `site_packages_resolution` — a fake venv layout with a stub package proves an import resolves
//!   through the host-resolved `site-packages`, and that the same project without the environment
//!   does not (the control that shows the first assertion can fail).
//! - the `backend` gate: the engine does nothing until `backend = "ty"` is set.
#![cfg(feature = "ty")]

use std::fs;
use std::path::{Path, PathBuf};

use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{
    Diagnostic, Engine, ProjectEnvironment, ProjectFile, ProjectRequest, PythonEnvironment, Severity,
};
use poly_engine_api::language::Language;
use poly_engines_python::TyEngine;

fn config(options: &[(&str, toml::Value)]) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: options.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
    }
}

fn ty_config() -> EngineConfig {
    config(&[("backend", toml::Value::String("ty".into()))])
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
    TyEngine.project_check(&ProjectRequest {
        root,
        files,
        config,
        environment,
        threads: None,
    })
}

/// `file:line:col code message`, one per finding, in report order.
fn summarize(root: &Path, reported: &[(PathBuf, Vec<Diagnostic>)]) -> Vec<String> {
    reported
        .iter()
        .flat_map(|(path, diagnostics)| {
            diagnostics.iter().map(move |d| {
                let span = d.span.as_ref().expect("ty findings carry a span");
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
    let reported = run(dir.path(), &files, &ty_config(), &ProjectEnvironment::default()).unwrap();

    assert_eq!(reported.len(), 1, "one file with findings: {reported:?}");
    assert_eq!(reported[0].0, dir.path().join("app.py"));
    let diagnostics = &reported[0].1;
    assert!(diagnostics.iter().all(|d| d.engine == "python-types"));
    assert!(diagnostics.iter().all(|d| d.severity == Severity::Error));
    let by_code: Vec<(&str, u32)> = diagnostics
        .iter()
        .map(|d| (d.code.as_deref().unwrap(), d.span.as_ref().unwrap().start_line))
        .collect();
    assert_eq!(by_code, [("invalid-argument-type", 5), ("invalid-assignment", 6)]);
    insta::assert_snapshot!(summarize(dir.path(), &reported).join("\n"));
}

#[test]
fn clean_project_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("ok.py", "x: int = 1\n")]);
    let reported = run(dir.path(), &files, &ty_config(), &ProjectEnvironment::default()).unwrap();
    assert!(reported.is_empty(), "{reported:?}");
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

    let resolved = run(dir.path(), &files, &ty_config(), &environment).unwrap();
    let codes: Vec<&str> = resolved
        .iter()
        .flat_map(|(_, ds)| ds.iter().map(|d| d.code.as_deref().unwrap()))
        .collect();
    assert_eq!(
        codes,
        ["invalid-assignment"],
        "the import resolved to `answer() -> int`, so assigning it to `str` is the only error: {resolved:?}"
    );

    let unresolved = run(dir.path(), &files, &ty_config(), &ProjectEnvironment::default()).unwrap();
    let codes: Vec<&str> = unresolved
        .iter()
        .flat_map(|(_, ds)| ds.iter().map(|d| d.code.as_deref().unwrap()))
        .collect();
    assert!(
        codes.contains(&"import-not-found"),
        "without the environment the import must not resolve: {unresolved:?}"
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
fn unknown_backend_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", "x = 1\n")]);
    let cfg = config(&[("backend", toml::Value::String("mypy".into()))]);
    let error = run(dir.path(), &files, &cfg, &ProjectEnvironment::default()).unwrap_err();
    assert!(error.to_string().contains("unknown `backend`"), "{error:#}");
}

#[test]
fn a_non_string_backend_is_an_error_that_prints_the_value() {
    let dir = tempfile::tempdir().unwrap();
    let files = project(dir.path(), &[("app.py", "x = 1\n")]);
    for (value, shown) in [
        (toml::Value::Integer(1), "unknown `backend` 1 for"),
        (toml::Value::Boolean(true), "unknown `backend` true for"),
        (
            toml::Value::Array(vec![toml::Value::String("ty".into())]),
            "unknown `backend` [\"ty\"] for",
        ),
    ] {
        let cfg = config(&[("backend", value)]);
        let error = run(dir.path(), &files, &cfg, &ProjectEnvironment::default()).unwrap_err();
        assert!(error.to_string().contains(shown), "{error:#}");
    }
}

#[test]
fn python_version_option_drives_version_dependent_checks() {
    let dir = tempfile::tempdir().unwrap();
    let source = "import tomllib\n";
    let files = project(dir.path(), &[("app.py", source)]);
    let old = config(&[
        ("backend", toml::Value::String("ty".into())),
        ("python_version", toml::Value::String("3.10".into())),
    ]);
    let reported = run(dir.path(), &files, &old, &ProjectEnvironment::default()).unwrap();
    let codes: Vec<&str> = reported
        .iter()
        .flat_map(|(_, ds)| ds.iter().map(|d| d.code.as_deref().unwrap()))
        .collect();
    assert_eq!(codes, ["import-not-found"], "tomllib is 3.11+: {reported:?}");

    let new = config(&[
        ("backend", toml::Value::String("ty".into())),
        ("python_version", toml::Value::String("3.12".into())),
    ]);
    assert!(
        run(dir.path(), &files, &new, &ProjectEnvironment::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn version_names_the_ty_crate() {
    assert!(TyEngine.version().contains("0.0.21"));
}
