//! `poly components` on the bundled binary: every engine family is linked, so nothing is
//! downloadable, the flavor is `bundled`, and `install` is a no-op that downloads nothing.
#![cfg(unix)]

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
        std::fs::write(repo.path().join("ok.rb"), "x = 1\n").expect("write ok.rb");
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

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: {error}: {}", String::from_utf8_lossy(&output.stderr)))
    }
}

#[test]
fn list_reports_every_family_as_bundled() {
    let value = Sandbox::new().json(&["components", "list", "--format", "json"]);
    assert_eq!(value["flavor"], "bundled");
    let components = value["components"].as_array().expect("components");
    assert!(!components.is_empty());
    // The `mcp` front end hosts no engines and is never linked; only engine components count.
    let engines: Vec<_> = components.iter().filter(|c| c["kind"] == "engines").collect();
    assert!(!engines.is_empty());
    assert!(engines.iter().all(|c| c["state"] == "bundled"), "{value}");
}

#[test]
fn status_needs_nothing_for_a_ruby_file() {
    let value = Sandbox::new().json(&["components", "status", "--format", "json", "ok.rb"]);
    assert_eq!(value["flavor"], "bundled");
    assert_eq!(value["missing"], serde_json::json!([]));
}

#[test]
fn install_all_is_a_noop_when_every_family_is_linked() {
    let sandbox = Sandbox::new();
    let value = sandbox.json(&["component", "install", "all", "--format", "json"]);
    assert_eq!(value["ok"], true);
    let results = value["results"].as_array().expect("results");
    assert!(!results.is_empty());
    assert!(results.iter().all(|r| r["action"] == "bundled"), "{value}");
    assert!(!sandbox.cache.path().join("sha256").exists());
}

#[test]
fn doctor_reports_the_bundled_flavor() {
    // `doctor` exits non-zero on unrelated host findings; only its components section matters here.
    let value = Sandbox::new().json(&["doctor", "--format", "json"]);
    assert_eq!(value["components"]["flavor"], "bundled");
    assert_eq!(value["components"]["needed_missing"], serde_json::json!([]));
}
