//! The v2 command surface: global flags, `--workspace auto|always|never`,
//! `--set`, the flags that were removed, and `--help` snapshots.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ok.py"), "x = 1\n").expect("write ok.py");
    dir
}

fn poly(root: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .args(args)
        .env_remove("POLY_OFFLINE")
        .current_dir(root)
        .output()
        .expect("run poly")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn help(args: &[&str]) -> String {
    let dir = repo();
    let output = poly(dir.path(), args);
    assert!(output.status.success(), "{args:?}: {}", stderr(&output));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn top_level_help() {
    insta::assert_snapshot!("top_level_help", help(&["--help"]));
}

#[test]
fn lint_help() {
    insta::assert_snapshot!("lint_help", help(&["lint", "--help"]));
}

#[test]
fn fmt_help() {
    insta::assert_snapshot!("fmt_help", help(&["fmt", "--help"]));
}

#[test]
fn workspace_accepts_auto_always_and_never() {
    let dir = repo();
    for value in ["auto", "always", "never"] {
        let output = poly(dir.path(), &["lint", "--workspace", value, "--no-cache", "ok.py"]);
        assert!(
            !stderr(&output).contains("invalid value"),
            "--workspace {value} must parse: {}",
            stderr(&output)
        );
    }
}

#[test]
fn workspace_never_skips_the_phase_without_a_note() {
    let dir = repo();
    let output = poly(dir.path(), &["lint", "--workspace", "never", "--no-cache", "."]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(!stderr(&output).contains("whole-project phase"), "{}", stderr(&output));
}

#[test]
fn workspace_rejects_other_values_and_the_old_forms() {
    let dir = repo();
    for args in [
        vec!["lint", "--workspace", "sometimes", "ok.py"],
        vec!["lint", "--workspace"],
        vec!["lint", "--no-workspace", "ok.py"],
        vec!["fmt", "--workspace", "never", "ok.py"],
    ] {
        let output = poly(dir.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{args:?} must be a usage error");
    }
}

#[test]
fn color_takes_three_values_and_replaces_no_color() {
    let dir = repo();
    for value in ["auto", "always", "never"] {
        let output = poly(dir.path(), &["--color", value, "fmt", "ok.py"]);
        assert_eq!(output.status.code(), Some(0), "--color {value}: {}", stderr(&output));
    }
    for args in [
        vec!["--color", "sometimes", "fmt", "ok.py"],
        vec!["fmt", "--no-color", "ok.py"],
    ] {
        assert_eq!(poly(dir.path(), &args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn color_never_strips_escapes_even_when_always_would_add_them() {
    let dir = repo();
    std::fs::write(dir.path().join("a.py"), "x=1\n").expect("write a.py");
    let always = poly(dir.path(), &["--color", "always", "fmt", "a.py"]);
    let never = poly(dir.path(), &["--color", "never", "fmt", "a.py"]);
    let text = |o: &Output| format!("{}{}", String::from_utf8_lossy(&o.stdout), stderr(o));
    assert!(
        text(&always).contains('\u{1b}'),
        "--color always must colour: {}",
        text(&always)
    );
    assert!(
        !text(&never).contains('\u{1b}'),
        "--color never must not: {}",
        text(&never)
    );
}

#[test]
fn global_flags_are_accepted_before_and_after_the_subcommand() {
    let dir = repo();
    for args in [
        vec!["--offline", "-q", "fmt", "ok.py"],
        vec!["fmt", "--offline", "-q", "ok.py"],
        vec!["-v", "fmt", "ok.py"],
        vec!["--debug", "fmt", "ok.py"],
        vec!["--config", "poly.toml", "doctor", "--format", "toon"],
    ] {
        let output = poly(dir.path(), &args);
        assert_ne!(output.status.code(), Some(2), "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn quiet_conflicts_with_verbose_and_debug() {
    let dir = repo();
    for other in ["-v", "--debug"] {
        let output = poly(dir.path(), &["fmt", "-q", other, "ok.py"]);
        assert_eq!(output.status.code(), Some(2), "-q {other}");
    }
}

#[test]
fn poly_offline_env_is_read_as_a_boolean() {
    let dir = repo();
    for value in ["1", "true", "0"] {
        let output = Command::new(POLY)
            .args(["fmt", "ok.py"])
            .env("POLY_OFFLINE", value)
            .current_dir(dir.path())
            .output()
            .expect("run poly");
        assert_eq!(
            output.status.code(),
            Some(0),
            "POLY_OFFLINE={value}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn on_missing_is_accepted_and_validated() {
    let dir = repo();
    for value in ["fail", "skip"] {
        let output = poly(
            dir.path(),
            &["lint", "--workspace", "never", "--on-missing", value, "ok.py"],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "--on-missing {value}: {}",
            stderr(&output)
        );
    }
    let output = poly(dir.path(), &["lint", "--on-missing", "maybe", "ok.py"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn set_overrides_poly_toml_for_one_run() {
    let dir = repo();
    // The config lives outside the walked tree: a `poly.toml` inside it would be a walked TOML file,
    // which the `web` component lints and this core binary has no lock to fetch it with.
    let config_dir = tempfile::tempdir().expect("config dir");
    let config = config_dir.path().join("poly.toml");
    std::fs::write(&config, "[discovery]\nexclude = [\"ok.py\"]\n").expect("write poly.toml");
    let config = config.to_str().expect("utf8 path");
    std::fs::write(dir.path().join("ok.py"), "x   =    1\n").expect("write ok.py");

    let excluded = poly(dir.path(), &["fmt", "--config", config, "."]);
    assert_eq!(excluded.status.code(), Some(0), "the config excludes the only file");

    let included = poly(
        dir.path(),
        &["fmt", "--config", config, "--set", "discovery.exclude=[]", "."],
    );
    assert_eq!(
        included.status.code(),
        Some(1),
        "--set must lift the exclude: {}",
        stderr(&included)
    );
}

#[test]
fn set_rejects_a_malformed_argument() {
    let dir = repo();
    let output = poly(dir.path(), &["fmt", "--set", "no-separator", "ok.py"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--set"), "{}", stderr(&output));
}

#[test]
fn removed_flags_are_rejected() {
    let dir = repo();
    for args in [
        vec!["lint", "--force-exclude", "ok.py"],
        vec!["lint", "--include-generated", "ok.py"],
        vec!["lint", "--skip-generated", "ok.py"],
        vec!["fmt", "--no-color", "ok.py"],
        vec!["doctor", "--no-color"],
        vec!["cache", "size"],
    ] {
        let output = poly(dir.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{args:?} must be a usage error");
    }
}

#[test]
fn doctor_renders_toon() {
    let dir = repo();
    let output = poly(dir.path(), &["doctor", "--format", "toon"]);
    assert_ne!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!output.stdout.is_empty());
}
