//! `poly rules` in a core build (no ast-grep engine family linked): the subcommand exists and its
//! inventory comes from the `astgrep` component.
//!
//! Run with `cargo test -p poly-cli --no-default-features --features dev-lock --test rules_core`;
//! the installed-component test also needs `mise run components:dev astgrep` and
//! `POLY_COMPONENT_DEV_LOCK` pointing at the lock it prints.
#![cfg(all(unix, not(feature = "engine-astgrep")))]

use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn poly(cwd: &TempDir, args: &[&str]) -> Output {
    std::fs::write(cwd.path().join("poly.toml"), "[rules]\ndirs = []\n").expect("write poly.toml");
    Command::new(POLY)
        .args(args)
        .current_dir(cwd.path())
        .env("POLY_COMPONENT_CACHE", cwd.path().join("cache"))
        .output()
        .expect("poly invocation")
}

#[test]
fn the_subcommand_exists_and_fails_closed_without_the_component() {
    let cwd = TempDir::new().unwrap();
    let output = poly(&cwd, &["--offline", "rules", "list"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.starts_with("poly rules: "),
        "an unknown subcommand would not say this: {stderr}"
    );
    // The component path names the `astgrep` component: offline with a dev lock, or no embedded lock
    // without one. The removed gate said the build lacked "the ast-grep engine family" instead.
    let reached_component = stderr.contains("component not installed: astgrep (offline)")
        || stderr.contains("this build embeds no component lock, so `astgrep` cannot be fetched");
    assert!(
        reached_component,
        "poly rules did not ask the astgrep component: {stderr}"
    );
}

#[cfg(feature = "dev-lock")]
#[test]
fn list_and_test_are_served_by_the_installed_component() {
    let cwd = TempDir::new().unwrap();
    let output = poly(&cwd, &["rules", "list", "--format", "json"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("a JSON document");
    assert_eq!(document["builtin_pack_enabled"], true);
    let rules = document["rules"].as_array().expect("rules array");
    assert!(
        rules
            .iter()
            .any(|rule| rule["id"] == "swallowed-error" && rule["source"] == "builtin"),
        "{document}"
    );

    let tested = poly(&cwd, &["rules", "test"]);
    assert!(tested.status.success(), "{}", String::from_utf8_lossy(&tested.stderr));
    assert!(String::from_utf8_lossy(&tested.stdout).contains("0 passed, 0 failed across 0 rules"));
}
