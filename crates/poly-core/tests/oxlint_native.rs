//! A repository's own `.oxlintrc.json[c]` decides which rules poly's oxc linter runs on JS/TS.

use std::fs;
use std::path::Path;

use poly_core::{Config, RunOptions};
use tempfile::tempdir;

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        only: vec!["oxc".to_string()],
        ..RunOptions::default()
    }
}

fn codes(root: &Path, file: &str) -> Vec<String> {
    let config = Config::load(root).expect("load config");
    let run = poly_core::lint_run(&[root.join(file)], &config, &options(), false, false).unwrap();
    run.results
        .iter()
        .flat_map(|result| &result.diagnostics)
        .filter_map(|diagnostic| diagnostic.code.clone())
        .collect()
}

fn repo(root_config: &str) -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    fs::create_dir(repo.path().join(".git")).unwrap();
    write(&repo.path().join(".oxlintrc.jsonc"), root_config);
    repo
}

const IF_WITHOUT_BRACES: &str = "export function f(a) {\n  if (a) return 1;\n  return 2;\n}\n";

#[test]
fn the_project_config_enables_rules_poly_does_not_run_by_default() {
    let repo = repo(r#"{ "rules": { "curly": "error" } }"#);
    write(&repo.path().join("a.js"), IF_WITHOUT_BRACES);
    assert_eq!(codes(repo.path(), "a.js"), ["curly"]);
}

#[test]
fn a_project_config_replaces_poly_defaults_instead_of_adding_to_them() {
    let repo = repo(r#"{ "rules": { "no-debugger": "off" } }"#);
    write(&repo.path().join("a.js"), "debugger;\n");
    assert!(
        codes(repo.path(), "a.js").is_empty(),
        "{:?}",
        codes(repo.path(), "a.js")
    );
}

#[test]
fn the_nearest_nested_config_wins_and_does_not_inherit_the_root() {
    let repo = repo(r#"{ "rules": { "curly": "error" } }"#);
    write(
        &repo.path().join("pkg/.oxlintrc.json"),
        r#"{ "rules": { "no-debugger": "error" } }"#,
    );
    write(
        &repo.path().join("pkg/a.js"),
        &format!("{IF_WITHOUT_BRACES}debugger;\n"),
    );
    write(&repo.path().join("a.js"), IF_WITHOUT_BRACES);
    assert_eq!(codes(repo.path(), "pkg/a.js"), ["no-debugger"]);
    assert_eq!(codes(repo.path(), "a.js"), ["curly"]);
}

#[test]
fn overrides_and_ignore_patterns_apply() {
    let repo = repo(
        r#"{ "rules": { "curly": "error" },
             "overrides": [{ "files": ["**/*.test.js"], "rules": { "curly": "off" } }],
             "ignorePatterns": ["generated/**"] }"#,
    );
    write(&repo.path().join("a.test.js"), IF_WITHOUT_BRACES);
    write(&repo.path().join("generated/a.js"), IF_WITHOUT_BRACES);
    assert!(codes(repo.path(), "a.test.js").is_empty());
    assert!(codes(repo.path(), "generated/a.js").is_empty());
}

#[test]
fn js_plugins_do_not_stop_the_other_rules_from_running() {
    let repo = repo(r#"{ "jsPlugins": ["./plugin.ts"], "rules": { "curly": "error", "custom/rule": "error" } }"#);
    write(&repo.path().join("plugin.ts"), "export default {};\n");
    write(&repo.path().join("a.js"), IF_WITHOUT_BRACES);
    assert_eq!(codes(repo.path(), "a.js"), ["curly"]);
}

#[test]
fn oxlintrc_false_returns_to_the_builtin_rule_set() {
    let repo = repo(r#"{ "rules": { "curly": "error" } }"#);
    write(
        &repo.path().join("poly.toml"),
        "[lint.javascript.oxc]\noxlintrc = false\n",
    );
    write(&repo.path().join("a.js"), IF_WITHOUT_BRACES);
    assert!(!codes(repo.path(), "a.js").contains(&"curly".to_string()));
}
