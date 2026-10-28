//! The Python version ruff lints for comes from the project's own ruff config
//! (Goldziher/poly#42).
//!
//! poly never read `[tool.ruff] target-version` or `requires-python`; it only
//! looked like it did while ruff's default target was 3.10. The 0.17 ruff line
//! moved that default to 3.11, and every 3.10 project started receiving
//! 3.11-only rewrites (`UP017` → `datetime.UTC`, `UP042` → `StrEnum`) and an
//! import order that files `tomllib` under the standard library.
//!
//! Each case lints a real file inside a temporary project, and each "honoured"
//! assertion is paired with a py311 counterpart that must still fire, so none
//! of them passes merely because the rule is off.

use std::path::Path;

use poly_core::{
    Language,
    config::{EngineConfig, GlobalDefaults},
    engine::{Engine, SourceFile},
    engines::ruff::RuffEngine,
};

const UTC_SNIPPET: &str = "\
import datetime


def now() -> datetime.datetime:
    return datetime.datetime.now(datetime.timezone.utc)
";

/// The import order a 3.10 project must use: `tomllib` is not standard
/// library there, so it sits in its own section.
const TOMLLIB_SNIPPET: &str = "\
import os

import tomllib

print(os, tomllib)
";

fn engine_cfg(options: &str) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::from_str(options).unwrap(),
    }
}

fn write(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// Lint `content` as `<dir>/<name>` and return the rule codes reported.
fn codes_at(dir: &Path, name: &str, content: &str, options: &str) -> Vec<String> {
    write(dir, name, content);
    let src = SourceFile {
        path: dir.join(name),
        language: Language::Python,
        content: content.into(),
    };
    RuffEngine
        .lint(&src, &engine_cfg(options))
        .unwrap()
        .into_iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect()
}

fn project(pyproject: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "pyproject.toml", pyproject);
    dir
}

fn has(codes: &[String], code: &str) -> bool {
    codes.iter().any(|c| c == code)
}

#[test]
fn tool_ruff_target_version_py310_suppresses_up017() {
    let dir =
        project("[project]\nname = \"p\"\nrequires-python = \">=3.10\"\n\n[tool.ruff]\ntarget-version = \"py310\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "py310 has no datetime.UTC: {codes:?}");
}

#[test]
fn tool_ruff_target_version_py311_still_reports_up017() {
    let dir = project("[tool.ruff]\ntarget-version = \"py311\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "");
    assert!(
        has(&codes, "UP017"),
        "py311 must still be told to use datetime.UTC: {codes:?}"
    );
}

#[test]
fn requires_python_is_the_target_when_target_version_is_unset() {
    let dir = project("[project]\nname = \"p\"\nrequires-python = \">=3.10\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "requires-python >=3.10 targets 3.10: {codes:?}");

    let dir = project("[project]\nname = \"p\"\nrequires-python = \">=3.11\"\n\n[tool.ruff.lint]\nselect = [\"E\"]\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "");
    assert!(has(&codes, "UP017"), "requires-python >=3.11 targets 3.11: {codes:?}");
}

#[test]
fn target_version_beats_requires_python() {
    let dir = project("[project]\nrequires-python = \">=3.11\"\n\n[tool.ruff]\ntarget-version = \"py310\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "{codes:?}");
}

#[test]
fn a_file_in_a_subdirectory_finds_the_project_config() {
    let dir = project("[project]\nrequires-python = \">=3.10\"\n");
    let codes = codes_at(dir.path(), "src/pkg/deep/m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "{codes:?}");
}

#[test]
fn ruff_toml_and_its_extend_chain_set_the_target() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "base.toml", "target-version = \"py310\"\n");
    write(
        dir.path(),
        "app/ruff.toml",
        "extend = \"../base.toml\"\nline-length = 100\n",
    );
    let codes = codes_at(dir.path(), "app/m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "the extended base sets py310: {codes:?}");

    write(dir.path(), "base.toml", "target-version = \"py312\"\n");
    write(dir.path(), "other/.ruff.toml", "extend = \"../base.toml\"\n");
    let codes = codes_at(dir.path(), "other/m.py", UTC_SNIPPET, "");
    assert!(has(&codes, "UP017"), "{codes:?}");
}

#[test]
fn the_nearest_ruff_config_wins_over_a_deeper_plain_pyproject() {
    let dir = project("[tool.ruff]\ntarget-version = \"py310\"\n");
    // A pyproject.toml with no [tool.ruff] is not a ruff config; its
    // requires-python must not displace the ruff config above it.
    write(
        dir.path(),
        "sub/pyproject.toml",
        "[project]\nrequires-python = \">=3.12\"\n",
    );
    let codes = codes_at(dir.path(), "sub/m.py", UTC_SNIPPET, "");
    assert!(!has(&codes, "UP017"), "{codes:?}");
}

#[test]
fn poly_toml_target_version_beats_the_project_config() {
    let dir = project("[tool.ruff]\ntarget-version = \"py310\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "target_version = \"py311\"");
    assert!(has(&codes, "UP017"), "{codes:?}");

    let dir = project("[tool.ruff]\ntarget-version = \"py312\"\n");
    let codes = codes_at(dir.path(), "m.py", UTC_SNIPPET, "target_version = \"py310\"");
    assert!(!has(&codes, "UP017"), "{codes:?}");
}

#[test]
fn isort_classifies_tomllib_by_the_target() {
    let dir = project("[project]\nrequires-python = \">=3.10\"\n");
    let codes = codes_at(dir.path(), "m.py", TOMLLIB_SNIPPET, "");
    assert!(!has(&codes, "I001"), "on 3.10 tomllib is third-party: {codes:?}");

    let dir = project("[project]\nrequires-python = \">=3.11\"\n");
    let codes = codes_at(dir.path(), "m.py", TOMLLIB_SNIPPET, "");
    assert!(has(&codes, "I001"), "on 3.11 tomllib is standard library: {codes:?}");
}

#[test]
fn the_cache_context_follows_the_discovered_target() {
    let dir = project("[tool.ruff]\ntarget-version = \"py310\"\n");
    let file = dir.path().join("m.py");
    let cfg = engine_cfg("");
    RuffEngine.run_context(&cfg);
    let before = RuffEngine.file_context(&file, &cfg);

    write(
        dir.path(),
        "pyproject.toml",
        "[tool.ruff]\ntarget-version = \"py311\"\n",
    );
    RuffEngine.run_context(&cfg);
    let after = RuffEngine.file_context(&file, &cfg);

    assert_ne!(
        before, after,
        "editing target-version must invalidate cached lint results"
    );
}
