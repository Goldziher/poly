//! End-to-end coverage for `poly config show`.
//!
//! The command used to print a section *summary* — `[lint]  python` named the
//! language and stopped — so there was no way to ask poly what it actually
//! parsed: a key poly discarded, and a value that an `extends` base or
//! `poly.local.toml` overrode, were both invisible (issue #16). These tests
//! shell out to the built binary (via `CARGO_BIN_EXE_poly`) so they exercise the
//! whole path: arg parsing → `extends` + cascade + local-override resolution →
//! effective-config rendering.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn config_show(cwd: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("config")
        .arg("show")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("poly invocation")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "poly config show failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repo directory holding `poly.toml`. `git` marks it as a repository root so
/// the hierarchical cascade (ADR 0018), not the single-file loader, resolves it.
fn repo(config: &str, git: bool) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("poly.toml"), config).expect("write poly.toml");
    if git {
        std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    }
    tmp
}

/// Parse `poly config show` output back as TOML — which also asserts that what
/// the command prints is a valid TOML document, i.e. diffable against the
/// `poly.toml` the user wrote.
fn effective_toml(stdout: &str) -> toml::Table {
    toml::from_str(stdout).unwrap_or_else(|error| panic!("output is not valid TOML: {error}\n---\n{stdout}"))
}

fn lookup<'a>(table: &'a toml::Table, path: &str) -> &'a toml::Value {
    let mut keys = path.split('.');
    let first = keys.next().expect("non-empty path");
    let mut value = table
        .get(first)
        .unwrap_or_else(|| panic!("missing key `{first}` in {table:?}"));
    for key in keys {
        value = value
            .get(key)
            .unwrap_or_else(|| panic!("missing key `{key}` of `{path}` in {value:?}"));
    }
    value
}

/// The issue's own case: a key nested three levels deep under `[lint]` must be
/// visible — key *and* value — so a user can diff what they wrote against what
/// poly kept.
#[test]
fn shows_nested_lint_engine_key_and_value() {
    let tmp = repo("[lint.python.ruff]\nmccabe_max_complexity = 3\n", false);
    let stdout = stdout_of(&config_show(tmp.path(), &[]));

    assert!(
        stdout.contains("mccabe_max_complexity"),
        "the key a user wrote must appear: {stdout}"
    );
    let document = effective_toml(&stdout);
    assert_eq!(
        lookup(&document, "lint.python.ruff.mccabe_max_complexity"),
        &toml::Value::Integer(3),
        "the effective value must be shown, not just the language name: {stdout}"
    );
}

/// The cascade a user cannot see at all today: an `extends` base contributes a
/// key, the declaring file overrides one, and `poly.local.toml` wins on top.
/// Every value shown must be the post-merge one.
#[test]
fn shows_post_merge_values_from_extends_base_and_local_override() {
    let tmp = repo(
        "extends = [\"base/poly.toml\"]\n\
         [lint.python.ruff]\n\
         line_length = 95\n",
        true,
    );
    std::fs::create_dir(tmp.path().join("base")).expect("create base dir");
    std::fs::write(
        tmp.path().join("base/poly.toml"),
        "[lint.python.ruff]\nline_length = 88\nselect = [\"E\"]\n",
    )
    .expect("write base config");
    std::fs::write(
        tmp.path().join("poly.local.toml"),
        "[lint.python.ruff]\nline_length = 100\n",
    )
    .expect("write local override");

    let document = effective_toml(&stdout_of(&config_show(tmp.path(), &[])));

    assert_eq!(
        lookup(&document, "lint.python.ruff.line_length"),
        &toml::Value::Integer(100),
        "poly.local.toml is the final layer, so 100 — not 95 or 88 — is effective"
    );
    assert_eq!(
        lookup(&document, "lint.python.ruff.select"),
        &toml::Value::Array(vec![toml::Value::String("E".to_string())]),
        "a key only the extends base sets must still be shown"
    );
}

/// Everything the old summary conveyed survives: the resolved `[defaults]`, the
/// accumulated `[discovery] exclude`, whether `[hooks]` is present, and what
/// `extends` resolved to.
#[test]
fn keeps_defaults_discovery_hooks_and_extends_information() {
    let tmp = repo(
        "extends = [\"base/poly.toml\"]\n\
         [discovery]\n\
         exclude = [\"vendor/**\"]\n\
         [hooks.pre-commit.commands.noop]\n\
         run = \"true\"\n",
        true,
    );
    std::fs::create_dir(tmp.path().join("base")).expect("create base dir");
    std::fs::write(tmp.path().join("base/poly.toml"), "[defaults]\nline_length = 100\n").expect("write base config");

    let stdout = stdout_of(&config_show(tmp.path(), &[]));
    let document = effective_toml(&stdout);

    assert_eq!(
        lookup(&document, "defaults.line_length"),
        &toml::Value::Integer(100),
        "the base's [defaults] must show through"
    );
    assert_eq!(
        lookup(&document, "defaults.line_ending"),
        &toml::Value::String("lf".to_string()),
        "a default nobody wrote must still be shown as resolved"
    );
    assert_eq!(
        lookup(&document, "discovery.exclude"),
        &toml::Value::Array(vec![toml::Value::String("vendor/**".to_string())])
    );
    assert!(
        stdout.contains("base/poly.toml"),
        "the resolved extends base must be reported: {stdout}"
    );
    assert!(stdout.contains("hooks"), "hook presence must remain visible: {stdout}");
}

/// `--format json` carries the same effective config as a machine-readable
/// document, so a script can diff it without reparsing TOML.
#[test]
fn json_format_carries_the_effective_config() {
    let tmp = repo("[lint.python.ruff]\nmccabe_max_complexity = 3\n", false);
    let stdout = stdout_of(&config_show(tmp.path(), &["--format", "json"]));

    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        document["config"]["lint"]["python"]["ruff"]["mccabe_max_complexity"],
        serde_json::json!(3)
    );
    assert_eq!(document["resolution"]["hooks_present"], serde_json::json!(false));
}

/// `--format toon` renders the same document; a machine consumer must never get
/// an empty payload.
#[test]
fn toon_format_carries_the_effective_config() {
    let tmp = repo("[lint.python.ruff]\nmccabe_max_complexity = 3\n", false);
    let stdout = stdout_of(&config_show(tmp.path(), &["--format", "toon"]));

    assert!(
        stdout.contains("mccabe_max_complexity"),
        "toon output must carry the effective key: {stdout}"
    );
}

/// An explicit `--config` resolves that file (and its sibling local override),
/// not whatever the working directory would have found.
#[test]
fn explicit_config_path_resolves_that_file() {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::create_dir(tmp.path().join("other")).expect("create other dir");
    std::fs::write(tmp.path().join("poly.toml"), "[lint.python.ruff]\nline_length = 70\n").expect("write poly.toml");
    std::fs::write(
        tmp.path().join("other/poly.toml"),
        "[lint.python.ruff]\nline_length = 111\n",
    )
    .expect("write other config");

    let document = effective_toml(&stdout_of(&config_show(tmp.path(), &["--config", "other/poly.toml"])));
    assert_eq!(
        lookup(&document, "lint.python.ruff.line_length"),
        &toml::Value::Integer(111)
    );
}

/// With no `poly.toml` anywhere, the dump is poly's built-in defaults and says
/// so — it must not name a config file that does not exist.
#[test]
fn reports_built_in_defaults_when_no_config_file_exists() {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".git")).expect("create .git");
    let stdout = stdout_of(&config_show(tmp.path(), &[]));

    assert!(
        stdout.contains("(none found"),
        "a missing config must be stated, not implied by a path: {stdout}"
    );
    let document = effective_toml(&stdout);
    assert_eq!(
        lookup(&document, "defaults.line_length"),
        &toml::Value::Integer(120),
        "the built-in defaults must still be shown"
    );
}

/// Two runs over the same tree must be byte-identical, or the output is useless
/// for diffing.
#[test]
fn output_is_deterministic_across_runs() {
    let tmp = repo(
        "[lint.python.ruff]\nselect = [\"E\", \"F\"]\n[fmt.python.ruff]\nquote_style = \"double\"\n",
        false,
    );
    let first = stdout_of(&config_show(tmp.path(), &[]));
    let second = stdout_of(&config_show(tmp.path(), &[]));
    assert_eq!(first, second);
}
