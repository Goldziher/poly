//! End-to-end coverage for `poly doctor` and the PATH-shadow warning.
//!
//! Four incidents motivated this surface, and all four shared one shape: the
//! report of success and the effect disagreed. `brew upgrade poly` reported an
//! upgrade while `poly --version` never moved (a cargo-installed binary was
//! first on `PATH`); a build reported a released version while carrying
//! unreleased fixes; a binary with an invalidated code signature exited
//! silently, indistinguishable from a hang.
//!
//! These shell out to the built binary because the guarantee lives at the
//! process boundary: what `PATH` resolves to, what the process prints on
//! stderr, and what it exits with.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// A `PATH` layout with the real binary and a stand-in "other install".
struct Installs {
    _dir: TempDir,
    /// A symlink to the real poly under test.
    real: PathBuf,
    /// A shell stub that reports a different version.
    other: PathBuf,
    /// Directory holding `real`.
    real_dir: PathBuf,
    /// Directory holding `other`.
    other_dir: PathBuf,
}

impl Installs {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let real_dir = dir.path().join("real");
        let other_dir = dir.path().join("other");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();

        // Symlinked, not copied: the built binary is large, and eleven copies of
        // it turn these tests into an I/O benchmark. `current_exe` resolves the
        // link, so the running binary still matches this PATH entry.
        let real = real_dir.join("poly");
        std::os::unix::fs::symlink(POLY, &real).expect("link the built poly");

        let other = other_dir.join("poly");
        std::fs::write(
            &other,
            "#!/bin/sh\necho 'poly 0.19.6 (release build v0.19.6, release)'\n",
        )
        .unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();

        Installs {
            _dir: dir,
            real,
            other,
            real_dir,
            other_dir,
        }
    }

    /// Run the real binary with a `PATH` made of the given directories.
    fn run(&self, path_dirs: &[&Path], args: &[&str]) -> Output {
        let path = path_dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        Command::new(&self.real)
            .args(args)
            .env("PATH", format!("{path}:/usr/bin:/bin"))
            .env_remove("POLY_NO_SHADOW_WARN")
            .output()
            .expect("run poly")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn version_reports_the_build_identity_not_just_the_number() {
    let output = Command::new(POLY).arg("--version").output().expect("run poly");
    let text = stdout(&output);
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "the version is still there: {text}"
    );
    // A bare `0.19.7` cannot distinguish the tagged release from a build with
    // unreleased fixes on top; the channel and build id can.
    assert!(text.contains("build"), "--version carries a build identifier: {text}");
    assert!(
        text.contains("dev") || text.contains("release") || text.contains("unknown"),
        "--version names a channel: {text}"
    );
}

#[test]
fn doctor_reports_the_running_binary_and_its_config() {
    let output = Command::new(POLY)
        .args(["doctor", "--no-color"])
        .output()
        .expect("run poly");
    let text = stdout(&output);
    assert!(text.contains("running executable"), "{text}");
    assert!(text.contains("poly on PATH"), "{text}");
    assert!(text.contains("config"), "{text}");
    assert!(text.contains("cache"), "{text}");
}

#[test]
fn doctor_emits_json_when_asked() {
    let output = Command::new(POLY)
        .args(["doctor", "--format", "json"])
        .output()
        .expect("run poly");
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("doctor --format json is valid JSON");
    assert!(parsed["running"]["version"].is_string(), "{parsed}");
    assert!(parsed["running"]["build_id"].is_string(), "{parsed}");
    assert!(parsed["path"]["installs"].is_array(), "{parsed}");
}

#[test]
fn doctor_fails_and_names_the_remedy_when_another_poly_is_ahead_on_path() {
    let installs = Installs::new();
    // The stand-in comes first: a bare `poly` would not run the binary under test.
    let output = installs.run(
        &[&installs.other_dir, &installs.real_dir],
        &["doctor", "--no-color", "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(1), "a shadowed install is a failure");

    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(parsed["path"]["shadowing"], 1, "one install is ahead: {parsed}");
    let findings = parsed["findings"].as_array().unwrap();
    let shadow = findings
        .iter()
        .find(|f| f["summary"].as_str().unwrap().contains("comes earlier on PATH"))
        .expect("the shadow is reported as a finding");
    assert_eq!(shadow["severity"], "error");
    assert!(
        shadow["remedy"].as_str().unwrap().starts_with("rm "),
        "the remedy is a concrete command: {shadow}"
    );
}

#[test]
fn doctor_reports_each_installs_version_including_the_hidden_one() {
    let installs = Installs::new();
    let output = installs.run(
        &[&installs.real_dir, &installs.other_dir],
        &["doctor", "--format", "json"],
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let reported: Vec<String> = parsed["path"]["installs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|install| install["version"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        reported.iter().any(|version| version.contains("0.19.6")),
        "the hidden install's own version is shown, which is what makes the mismatch visible: {reported:?}"
    );
    assert_eq!(parsed["path"]["shadowed"], 1, "one install is hidden: {parsed}");
}

#[test]
fn doctor_flags_an_install_that_cannot_report_its_version() {
    // The broken-code-signature shape: on PATH, executable, exits non-zero with
    // no output at all.
    let installs = Installs::new();
    std::fs::write(&installs.other, "#!/bin/sh\nexit 137\n").unwrap();
    std::fs::set_permissions(&installs.other, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = installs.run(
        &[&installs.real_dir, &installs.other_dir],
        &["doctor", "--format", "json"],
    );
    assert_eq!(output.status.code(), Some(1), "an unrunnable poly is a failure");
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let broken = parsed["path"]["installs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|install| install["outcome"] == "failed")
        .expect("the broken install is recorded as a failed probe");
    assert!(
        broken["detail"].as_str().unwrap().contains("137"),
        "the exit status is reported rather than swallowed: {broken}"
    );
}

#[test]
fn a_conflicting_path_warns_on_stderr_even_for_bare_version() {
    let installs = Installs::new();
    // `poly --version` is exactly the command that produced two false bug
    // reports, so it must carry the warning.
    let output = installs.run(&[&installs.other_dir, &installs.real_dir], &["--version"]);
    let warning = stderr(&output);
    assert!(
        warning.contains("comes earlier on PATH"),
        "the warning fires for --version: {warning}"
    );
    assert!(
        warning.contains("poly doctor"),
        "the warning points at the full diagnosis: {warning}"
    );
    assert!(
        stdout(&output).contains(env!("CARGO_PKG_VERSION")),
        "stdout is untouched by the warning"
    );
}

#[test]
fn a_hidden_install_warns_that_upgrading_it_changes_nothing() {
    let installs = Installs::new();
    let output = installs.run(&[&installs.real_dir, &installs.other_dir], &["--version"]);
    let warning = stderr(&output);
    assert!(
        warning.contains("hides another install"),
        "the reverse direction is the `brew upgrade` incident: {warning}"
    );
    assert!(
        warning.contains("will not change what `poly` runs"),
        "it says why the upgrade appeared to do nothing: {warning}"
    );
}

#[test]
fn a_single_install_never_warns() {
    let installs = Installs::new();
    let output = installs.run(&[&installs.real_dir], &["--version"]);
    assert_eq!(
        stderr(&output),
        "",
        "a correctly-installed poly prints no warning at all"
    );
}

#[test]
fn doctor_does_not_duplicate_the_warning_it_already_reports_in_full() {
    let installs = Installs::new();
    let output = installs.run(&[&installs.other_dir, &installs.real_dir], &["doctor", "--no-color"]);
    assert!(
        !stderr(&output).contains("run `poly doctor`"),
        "doctor does not tell you to run doctor: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("comes earlier on PATH"),
        "it reports the same conflict in full instead"
    );
}

#[test]
fn the_warning_can_be_silenced() {
    let installs = Installs::new();
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        installs.other_dir.display(),
        installs.real_dir.display()
    );
    let output = Command::new(&installs.real)
        .arg("--version")
        .env("PATH", path)
        .env("POLY_NO_SHADOW_WARN", "1")
        .output()
        .expect("run poly");
    assert_eq!(stderr(&output), "", "POLY_NO_SHADOW_WARN silences the warning");
}
