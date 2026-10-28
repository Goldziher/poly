//! A repository's own `.oxfmtrc.json[c]` configures poly's oxfmt-equivalent formatters, and its
//! `ignorePatterns` keep the files oxfmt does not own out of the run.

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

fn read(root: &Path, name: &str) -> String {
    fs::read_to_string(root.join(name)).unwrap()
}

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        ..RunOptions::default()
    }
}

fn format_repo(root: &Path) {
    let config = Config::load(root).expect("load config");
    poly_core::format(&[root.to_path_buf()], &config, &options(), true, false).unwrap();
}

const RC: &str = r#"// repo formatter config
{
  "printWidth": 120,
  "tabWidth": 4,
  "singleQuote": true,
  "trailingComma": "es5",
  "overrides": [{ "files": ["**/*.json", "**/*.yaml"], "options": { "tabWidth": 2 } }],
  "ignorePatterns": ["**/*.md", "generated/**"],
}
"#;

fn repo() -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    fs::create_dir(repo.path().join(".git")).unwrap();
    write(&repo.path().join(".oxfmtrc.jsonc"), RC);
    repo
}

#[test]
fn javascript_follows_the_repository_oxfmt_config() {
    let repo = repo();
    write(
        &repo.path().join("a.ts"),
        "export const f = (a: number) => { return {x: \"one\", y: [1,\n2,]} }\n",
    );
    format_repo(repo.path());
    assert_eq!(
        read(repo.path(), "a.ts"),
        "export const f = (a: number) => {\n    return { x: 'one', y: [1, 2] };\n};\n"
    );
}

#[test]
fn extension_overrides_reach_their_language() {
    let repo = repo();
    write(
        &repo.path().join("a.json"),
        "{\"a\": {\"b\": [1, 2, {\"c\": 3, \"d\": 4, \"e\": \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"}]}}\n",
    );
    write(&repo.path().join("a.yaml"), "a:\n      b: 1\n");
    format_repo(repo.path());
    assert!(
        read(repo.path(), "a.json").contains("\n  \"a\": {\n    \"b\": ["),
        "{}",
        read(repo.path(), "a.json")
    );
    assert_eq!(read(repo.path(), "a.yaml"), "a:\n  b: 1\n");
}

#[test]
fn ignore_patterns_keep_oxfmt_languages_out_of_the_run() {
    let repo = repo();
    let loose = "a:   1\n";
    write(&repo.path().join("generated/data.yaml"), loose);
    write(&repo.path().join("other/data.yaml"), loose);
    let ugly_markdown = "# T\n* a\n* b\n";
    write(&repo.path().join("README.md"), ugly_markdown);
    format_repo(repo.path());
    assert_eq!(read(repo.path(), "generated/data.yaml"), loose);
    assert_eq!(read(repo.path(), "README.md"), ugly_markdown);
    assert_eq!(read(repo.path(), "other/data.yaml"), "a: 1\n");
}

#[test]
fn an_explicit_poly_toml_key_wins_over_the_oxfmt_config() {
    let repo = repo();
    write(
        &repo.path().join("poly.toml"),
        "[fmt.typescript.oxc]\nquote_style = \"double\"\n",
    );
    write(&repo.path().join("a.ts"), "export const s = 'one';\n");
    format_repo(repo.path());
    assert_eq!(read(repo.path(), "a.ts"), "export const s = \"one\";\n");
}

#[test]
fn sort_imports_from_the_config_orders_imports() {
    let repo = tempdir().unwrap();
    fs::create_dir(repo.path().join(".git")).unwrap();
    write(
        &repo.path().join(".oxfmtrc.json"),
        r#"{"sortImports": {"newlinesBetween": true, "internalPattern": ["@/"], "groups": ["builtin", "external", "internal", ["parent", "sibling", "index"]]}}"#,
    );
    write(
        &repo.path().join("a.ts"),
        "import b from './b';\nimport z from 'zod';\nimport fs from 'node:fs';\nimport u from '@/util';\n",
    );
    format_repo(repo.path());
    assert_eq!(
        read(repo.path(), "a.ts"),
        "import fs from \"node:fs\";\n\nimport z from \"zod\";\n\nimport u from \"@/util\";\n\nimport b from \"./b\";\n"
    );
}
