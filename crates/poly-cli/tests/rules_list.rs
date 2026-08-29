//! End-to-end coverage for `poly rules list`.
//!
//! poly ships a built-in ast-grep rule pack embedded in the binary. Before this
//! surface existed, a user who got a `force-cast` or `swallowed-error` warning
//! had no command that would tell them where it came from or how to turn it
//! off: the listing enumerated only user rules from `[rules] dirs`. These tests
//! shell out to the built binary (via `CARGO_BIN_EXE_poly`), so they exercise
//! the whole path: arg parsing → config resolution → rule merge/selection →
//! human and machine output.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

fn rules_list(cwd: &Path, args: &[&str]) -> Output {
    Command::new(POLY)
        .arg("rules")
        .arg("list")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("poly invocation")
}

fn stdout_of(output: &Output) -> String {
    assert!(
        output.status.success(),
        "poly rules list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repo with a `poly.toml` and no user rules at all.
fn repo(config: &str) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(tmp.path().join("poly.toml"), config).expect("write poly.toml");
    tmp
}

#[test]
fn lists_builtin_pack_rules_with_source_language_and_severity() {
    let tmp = repo("[rules]\ndirs = []\n");
    let stdout = stdout_of(&rules_list(tmp.path(), &[]));

    assert!(
        stdout.contains("swallowed-error"),
        "an on-by-default pack rule must be listed: {stdout}"
    );
    let row = stdout
        .lines()
        .find(|line| line.starts_with("swallowed-error"))
        .expect("swallowed-error row");
    for field in ["rust", "builtin", "warning"] {
        assert!(row.contains(field), "row `{row}` should carry `{field}`");
    }
    assert!(
        stdout.contains("26 rule(s): 26 built-in, 0 user"),
        "the summary must count the pack: {stdout}"
    );
}

/// An opt-in (`severity: off`) rule is listed rather than hidden — otherwise
/// there is no way to discover that it exists to be turned on.
#[test]
fn lists_off_rules_and_marks_them_off() {
    let tmp = repo("[rules]\ndirs = []\n");
    let stdout = stdout_of(&rules_list(tmp.path(), &[]));
    let row = stdout
        .lines()
        .find(|line| line.starts_with("unwrap-used"))
        .unwrap_or_else(|| panic!("unwrap-used row missing: {stdout}"));
    assert!(row.contains("off"), "an opt-in rule must read as off: {row}");
}

#[test]
fn builtin_false_removes_the_pack_from_the_listing() {
    let tmp = repo("[rules]\ndirs = []\nbuiltin = false\n");
    let stdout = stdout_of(&rules_list(tmp.path(), &[]));
    assert!(
        !stdout.contains("swallowed-error"),
        "`[rules] builtin = false` must remove the pack: {stdout}"
    );
    assert!(
        stdout.contains("built-in rule pack is disabled"),
        "the empty listing must say why it is empty: {stdout}"
    );
}

/// `ignore` and a per-rule `level` move rules in the listing, so it reports the
/// config the run actually uses rather than the raw pack.
#[test]
fn listing_reflects_ignore_and_level_overrides() {
    let tmp = repo(concat!(
        "[rules]\ndirs = []\n\n",
        "[lint.astgrep]\nignore = [\"force-cast\"]\n\n",
        "[lint.astgrep.rules.swallowed-error]\nlevel = \"error\"\n",
    ));
    let stdout = stdout_of(&rules_list(tmp.path(), &[]));

    let ignored = stdout
        .lines()
        .find(|line| line.starts_with("force-cast"))
        .unwrap_or_else(|| panic!("force-cast row missing: {stdout}"));
    assert!(
        ignored.contains("off") && ignored.contains("warning"),
        "an ignored rule reports off, but keeps its declared default: {ignored}"
    );

    let promoted = stdout
        .lines()
        .find(|line| line.starts_with("swallowed-error"))
        .unwrap_or_else(|| panic!("swallowed-error row missing: {stdout}"));
    assert!(
        promoted.contains("error"),
        "a `level` override must show as the effective severity: {promoted}"
    );
    assert!(
        stdout.contains("12 enabled"),
        "ignoring one of the 13 default-on rules leaves 12: {stdout}"
    );
}

/// A user rule sharing a pack rule's id replaces it — one row, attributed to
/// the user, matching how `resolve_rules` merges the two.
#[test]
fn a_user_rule_overriding_a_pack_rule_is_listed_once_as_the_users() {
    let tmp = repo("[rules]\ndirs = [\".poly/rules\"]\n");
    let rules_dir = tmp.path().join(".poly/rules");
    std::fs::create_dir_all(&rules_dir).expect("create rule dir");
    std::fs::write(
        rules_dir.join("swallowed-error.yml"),
        "id: swallowed-error\nlanguage: rust\nseverity: error\nmessage: ours\nrule:\n  pattern: ours()\n",
    )
    .expect("write user rule");

    let stdout = stdout_of(&rules_list(tmp.path(), &[]));
    let rows: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("swallowed-error"))
        .collect();
    assert_eq!(rows.len(), 1, "one row per id, not one per source: {rows:?}");
    assert!(rows[0].contains("user"), "the user's rule wins the row: {}", rows[0]);
    assert!(
        stdout.contains("25 built-in, 1 user"),
        "the override replaces a pack rule rather than adding to it: {stdout}"
    );
}

/// The machine-readable document carries every field the table shows.
#[test]
fn json_output_carries_the_same_fields_as_the_table() {
    let tmp = repo("[rules]\ndirs = []\n");
    let stdout = stdout_of(&rules_list(tmp.path(), &["--format", "json"]));
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON document");

    assert_eq!(document["builtin_pack_enabled"], serde_json::json!(true));
    let rules = document["rules"].as_array().expect("rules array");
    assert_eq!(rules.len(), 26, "every pack rule is in the JSON document");

    let rule = rules
        .iter()
        .find(|rule| rule["id"] == "swallowed-error")
        .expect("pack rule in JSON");
    assert_eq!(rule["language"], "rust");
    assert_eq!(rule["source"], "builtin");
    assert_eq!(rule["default_severity"], "warning");
    assert_eq!(rule["severity"], "warning");
    assert_eq!(rule["enabled"], serde_json::json!(true));

    let off_rule = rules
        .iter()
        .find(|rule| rule["id"] == "unwrap-used")
        .expect("opt-in pack rule in JSON");
    assert_eq!(off_rule["default_severity"], "off");
    assert_eq!(off_rule["severity"], "off");
    assert_eq!(off_rule["enabled"], serde_json::json!(false));
}

#[test]
fn toon_output_is_rendered_as_a_table() {
    let tmp = repo("[rules]\ndirs = []\n");
    let stdout = stdout_of(&rules_list(tmp.path(), &["--format", "toon"]));
    assert!(
        stdout.contains("rules[26]{id,language,source,default_severity,severity,enabled}:"),
        "TOON header must name every field: {stdout}"
    );
    assert!(
        stdout.contains("swallowed-error,rust,builtin,warning,warning,true"),
        "TOON rows carry the same values: {stdout}"
    );
}
