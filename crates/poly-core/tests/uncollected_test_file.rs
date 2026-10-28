//! `uncollected-test-file` through the full lint pipeline (Goldziher/poly#39).
//!
//! A tree of test-shaped names across Python, Go, Ruby, TypeScript and shell,
//! linted twice: once with no configuration (the runner defaults, on for
//! Python, Go and Ruby only), and once with a `poly.toml` that replaces the
//! Python patterns, forbids shell tests and switches TypeScript on. Each run
//! snapshots exactly which files the rule names, so a helper or a collected
//! name gaining a finding shows up as a snapshot diff.

use std::path::Path;

use poly_core::{Config, LintRun, RunOptions};

const FILES: &[&str] = &[
    // Python: collected, uncollected, helpers.
    "tests/test_parser.py",
    "tests/parser_test.py",
    "tests/check_parser.py",
    "tests/parser.test.py",
    "tests/kebab-name.test.py",
    "tests/conftest.py",
    "tests/__init__.py",
    "tests/helpers/shared.test.py",
    "tests/fixtures/input.test.py",
    // Go.
    "pkg/parser_test.go",
    "pkg/parser.test.go",
    "pkg/testdata/input.test.go",
    // Ruby.
    "spec/parser_spec.rb",
    "spec/parser.spec.rb",
    "spec/spec_helper.rb",
    // TypeScript: off by default.
    "web/parser.test.ts",
    "web/test/parser_test.ts",
    // Shell: unchecked until configured; `.bats` is not shell.
    "tests/install_test.sh",
    "tests/test-install.bats",
];

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for file in FILES {
        let path = dir.path().join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let content = if file.ends_with(".go") { "package pkg\n" } else { "\n" };
        std::fs::write(path, content).expect("write fixture");
    }
    dir
}

fn lint(dir: &Path, config: &Config) -> LintRun {
    let options = RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    };
    poly_core::lint_run(&[dir.to_path_buf()], config, &options, false, false).expect("lint run")
}

/// `(forward-slash relative path, title)` for every `uncollected-test-file` finding.
fn findings(dir: &Path, run: &LintRun) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = run
        .results
        .iter()
        .flat_map(|result| {
            let rel = result
                .path
                .strip_prefix(dir)
                .expect("result under the fixture root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            result
                .diagnostics
                .iter()
                .filter(|d| d.code.as_deref() == Some("uncollected-test-file"))
                .map(move |d| (rel.clone(), d.title.clone()))
        })
        .collect();
    found.sort();
    found
}

#[test]
fn runner_defaults_flag_only_uncollectable_python_go_and_ruby_names() {
    let dir = tree();
    let run = lint(dir.path(), &Config::default());
    insta::assert_debug_snapshot!("uncollected_test_file_defaults", findings(dir.path(), &run));
}

#[test]
fn configured_patterns_forbid_and_opt_in_replace_the_defaults() {
    let dir = tree();
    let config_path = dir.path().join("poly.toml");
    std::fs::write(
        &config_path,
        r#"
[lint.python.quality]
test_file_patterns = ["check_*.py", "test_*.py"]

[lint.shell.quality]
forbid_test_files = true

[lint.typescript.quality]
uncollected_test_file = true

[lint.ruby.quality]
uncollected_test_file = false
"#,
    )
    .expect("write poly.toml");
    let config = Config::load_file(&config_path).expect("load poly.toml");
    let run = lint(dir.path(), &config);
    insta::assert_debug_snapshot!("uncollected_test_file_configured", findings(dir.path(), &run));
}
