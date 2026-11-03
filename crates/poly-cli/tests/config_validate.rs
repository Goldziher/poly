//! End-to-end coverage for `poly config validate`: the verdict comes from the compiled component
//! index, so it needs no installed component and never downloads one.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo(config: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("poly.toml"), config).expect("write poly.toml");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    tmp
}

fn validate(cwd: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(["config", "validate"])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("poly invocation")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn valid_config_exits_zero() {
    let tmp = repo("[engines.mago.lint]\nphp_version = \"8.3\"\n");
    let output = validate(tmp.path(), &[]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout_of(&output).contains("config is valid"), "{}", stdout_of(&output));
}

#[test]
fn typo_in_an_engine_table_exits_two_and_names_the_key() {
    let tmp = repo("[engines.mago.lint]\nphp_verison = \"8.3\"\n");
    let output = validate(tmp.path(), &[]);
    assert_eq!(output.status.code(), Some(2));
    let stdout = stdout_of(&output);
    assert!(
        stdout.contains("php_verison") && stdout.contains("php_version"),
        "{stdout}"
    );
}

#[test]
fn engine_of_a_disabled_component_exits_two() {
    let tmp = repo("[components]\ndisable = [\"php\"]\n\n[engines.mago]\nenabled = true\n");
    let output = validate(tmp.path(), &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout_of(&output).contains("disabled"), "{}", stdout_of(&output));
}

#[test]
fn a_linked_engine_checks_derived_keys_and_reports_nothing_partial() {
    // This binary links mago, so its serde-derived format keys are probed in-process; the
    // "partially validated" listing applies to a core-only build (see poly-core's
    // `config_validate` tests).
    let ok = validate(repo("[engines.mago.format]\nsort-uses = true\n").path(), &[]);
    assert_eq!(ok.status.code(), Some(0), "{}", stdout_of(&ok));
    assert!(!stdout_of(&ok).contains("partially validated"), "{}", stdout_of(&ok));

    let typo = validate(repo("[engines.mago.format]\nsort-use = true\n").path(), &[]);
    assert_eq!(typo.status.code(), Some(2), "{}", stdout_of(&typo));
    assert!(stdout_of(&typo).contains("sort-use"), "{}", stdout_of(&typo));
}

#[test]
fn a_directory_path_selects_its_poly_toml() {
    let tmp = repo("[engines.mago.lint]\nphp_verison = \"8.3\"\n");
    let elsewhere = TempDir::new().expect("tempdir");
    let output = validate(elsewhere.path(), &[tmp.path().to_str().expect("utf-8 path")]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_missing_config_file_exits_two() {
    let tmp = TempDir::new().expect("tempdir");
    let output = validate(tmp.path(), &["absent.toml"]);
    assert_eq!(output.status.code(), Some(2));
}
