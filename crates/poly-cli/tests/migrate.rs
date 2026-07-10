//! End-to-end `poly migrate` tests over throwaway fixture repos.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use poly_cli::MigrateArgs;
use poly_cli::migrate::{build_plan, run_migrate};
use tempfile::tempdir;

/// Build `MigrateArgs` for `dir`. `write` toggles apply vs report; `allow_dirty`
/// is always on so the tests never depend on the ambient git state.
fn args(dir: &Path, write: bool) -> MigrateArgs {
    MigrateArgs {
        path: Some(dir.to_path_buf()),
        write,
        report: !write,
        recurse: false,
        verify: false,
        allow_dirty: true,
        strip_superseded: false,
    }
}

#[test]
fn report_on_rust_repo_writes_nothing_and_keeps_rustfmt() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(dir.path().join("rustfmt.toml"), "max_width = 100\n").unwrap();

    let plan = build_plan(dir.path()).unwrap();
    assert!(
        plan.results.iter().all(|r| !r.has_fragments()),
        "a pure Rust repo has no absorbable configs"
    );
    assert!(
        plan.kept.iter().any(|action| action.path().ends_with("rustfmt.toml")),
        "rustfmt.toml must be on the KEEP list"
    );

    let code = run_migrate(args(dir.path(), false));
    assert!(matches!(code, ExitCode::SUCCESS) || format!("{code:?}").contains("SUCCESS"));
    assert!(
        !dir.path().join("poly.toml").exists(),
        "report mode must not write poly.toml"
    );
    assert!(dir.path().join("rustfmt.toml").exists(), "rustfmt.toml must remain");
}

#[test]
fn write_on_python_repo_absorbs_and_strips() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("pyproject.toml"),
        r#"[project]
name = "demo"
version = "0.1.0"

[tool.black]
line-length = 100

[tool.ruff]
line-length = 100

[tool.ruff.lint]
select = ["E", "F", "I"]
ignore = ["E501"]

[tool.ruff.lint.per-file-ignores]
"tests/**" = ["S101"]
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("_typos.toml"),
        "[default]\nextend-ignore-words = [\"ba\", \"crate\"]\n\n[default.extend-identifiers]\nHASHs = \"HASHs\"\n",
    )
    .unwrap();

    run_migrate(args(dir.path(), true));

    let poly = fs::read_to_string(dir.path().join("poly.toml")).expect("poly.toml written");
    insta::assert_snapshot!("python_repo_poly_toml", poly);

    assert!(
        !dir.path().join("_typos.toml").exists(),
        "_typos.toml should be deleted"
    );

    let pyproject_text = fs::read_to_string(dir.path().join("pyproject.toml")).unwrap();
    let parsed: toml::Table = toml::from_str(&pyproject_text).expect("pyproject still parses");
    assert!(parsed.contains_key("project"), "[project] must survive");
    let tool = parsed.get("tool").and_then(|t| t.as_table()).unwrap();
    assert!(tool.contains_key("black"), "[tool.black] must survive");
    assert!(!tool.contains_key("ruff"), "[tool.ruff] must be stripped");
}

#[test]
fn write_is_idempotent() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("ruff.toml"),
        "select = [\"E\", \"F\"]\nignore = [\"E501\"]\n",
    )
    .unwrap();

    run_migrate(args(dir.path(), true));
    let first = fs::read_to_string(dir.path().join("poly.toml")).unwrap();
    assert!(!dir.path().join("ruff.toml").exists(), "ruff.toml absorbed and deleted");

    run_migrate(args(dir.path(), true));
    let second = fs::read_to_string(dir.path().join("poly.toml")).unwrap();
    assert_eq!(first, second, "re-running --write must be idempotent");
}
