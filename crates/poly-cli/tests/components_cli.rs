//! `poly components` on the default (bundled) build: every engine family is linked, so nothing is
//! downloadable, `install` is a no-op and `list`/`status` need no lock.
#![cfg(all(unix, feature = "all-engines"))]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

struct Sandbox {
    repo: TempDir,
    cache: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let repo = tempfile::tempdir().expect("tempdir");
        std::fs::write(repo.path().join("ok.py"), "x = 1\n").expect("write ok.py");
        Self {
            repo,
            cache: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(POLY)
            .args(args)
            .env_remove("POLY_OFFLINE")
            .env_remove("POLY_COMPONENT_DEV_LOCK")
            .env_remove("POLY_COMPONENT_MIRROR")
            .env("POLY_COMPONENT_CACHE", self.cache.path())
            .current_dir(self.repo.path())
            .output()
            .expect("run poly")
    }

    /// Stdout with the machine-specific cache directory replaced.
    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        redact(&String::from_utf8_lossy(&output.stdout), self.cache.path())
    }
}

fn redact(text: &str, cache: &Path) -> String {
    let canonical = cache.canonicalize().unwrap_or_else(|_| cache.to_path_buf());
    text.replace(&canonical.display().to_string(), "<cache>")
        .replace(&cache.display().to_string(), "<cache>")
}

#[test]
fn list_pretty() {
    insta::assert_snapshot!("list_pretty", Sandbox::new().stdout(&["components", "list"]));
}

#[test]
fn list_json() {
    let sandbox = Sandbox::new();
    let json = sandbox.stdout(&["components", "list", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["flavor"], "bundled");
    insta::assert_snapshot!("list_json", json);
}

#[test]
fn status_pretty() {
    insta::assert_snapshot!("status_pretty", Sandbox::new().stdout(&["components", "status"]));
}

#[test]
fn status_json() {
    let sandbox = Sandbox::new();
    let json = sandbox.stdout(&["components", "status", "--format", "json", "ok.py"]);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(value["schema"], 1);
    assert_eq!(value["missing"], serde_json::json!([]));
    insta::assert_snapshot!("status_json", json);
}

#[test]
fn toon_carries_the_schema() {
    let toon = Sandbox::new().stdout(&["components", "list", "--format", "toon"]);
    assert!(toon.contains("schema: 1"), "{toon}");
}

#[test]
fn install_all_is_a_noop_on_the_bundled_build() {
    let sandbox = Sandbox::new();
    let json = sandbox.stdout(&["component", "install", "all", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(value["ok"], true);
    let results = value["results"].as_array().expect("results");
    assert!(!results.is_empty());
    assert!(results.iter().all(|r| r["action"] == "bundled"), "{json}");
    assert!(!sandbox.cache.path().join("sha256").exists());
}

#[test]
fn unknown_component_is_a_usage_error() {
    let output = Sandbox::new().run(&["components", "install", "no-such-component"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("no-such-component"));
}

#[test]
fn prune_without_a_lock_fails_instead_of_pretending() {
    let output = Sandbox::new().run(&["components", "prune", "--dry-run"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn doctor_json_has_a_components_section() {
    let output = Sandbox::new().run(&["doctor", "--format", "json"]);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    assert_eq!(value["components"]["flavor"], "bundled");
    assert_eq!(value["components"]["needed_missing"], serde_json::json!([]));
}
