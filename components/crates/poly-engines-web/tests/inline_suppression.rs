//! End-to-end inline suppression directives (ADR 0028) for the oxlint-backed languages, driven through
//! the real `lint` pipeline rather than the parser alone.
//!
//! Every language case is a *pair*: the same source without the directive must
//! report the diagnostic, and with the directive must not. Asserting only the
//! absence would pass just as happily against a backend that had stopped
//! reporting anything at all — which is the exact failure mode ADR 0028 calls
//! out (a suppression comment that is documented but read by no code).

use std::fs;
use std::path::Path;

use poly_core::{Config, RunOptions, lint};

mod common;

fn opts() -> RunOptions {
    RunOptions {
        force_exclude: false,
        fix_generated: false,
        generated: None,
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: true,
        config_resolver: None,
        externally_linted_languages: Vec::new(),
        only: Vec::new(),
        skip: Vec::new(),
        ..RunOptions::default()
    }
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Typos is opt-in, so the tests that read its findings enable it explicitly.
fn typos_enabled() -> Config {
    Config {
        lint: toml::from_str("[typos]\nenabled = true\n").expect("valid lint config"),
        ..Config::default()
    }
}

/// Lint a single file with the given contents in a throwaway tree and return the
/// codes it reports. The `tempfile::TempDir` is returned alongside so callers
/// that need the file afterwards can keep it alive.
fn codes_for(name: &str, contents: &str) -> Vec<String> {
    common::register();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    write(&path, contents);
    let results = lint(&[dir.path().to_path_buf()], &typos_enabled(), &opts(), false, false).unwrap();
    results
        .iter()
        .flat_map(|result| result.diagnostics.iter())
        .filter_map(|diagnostic| diagnostic.code.clone())
        .collect()
}

/// Assert that `code` is reported for `without` and gone for `with` — the pair
/// that proves the directive is what removed it.
fn assert_suppresses(name: &str, code: &str, without: &str, with: &str) {
    let before = codes_for(name, without);
    assert!(
        before.iter().any(|reported| reported == code),
        "{name}: expected {code} without a directive; got {before:?}"
    );
    let after = codes_for(name, with);
    assert!(
        !after.iter().any(|reported| reported == code),
        "{name}: {code} must be suppressed by the directive; got {after:?}"
    );
}

/// TypeScript, `//` comment: oxlint's `no-debugger`.
#[test]
fn typescript_slash_directive_suppresses_oxlint_no_debugger() {
    assert_suppresses(
        "app.ts",
        "no-debugger",
        "export function stop(): void {\n    debugger;\n}\n",
        "export function stop(): void {\n    debugger; // poly: allow[no-debugger] deliberate breakpoint in the debug build\n}\n",
    );
}

/// JavaScript, `/* … */` block comment on its own line, covering the next line.
#[test]
fn javascript_block_comment_directive_suppresses_the_next_line() {
    assert_suppresses(
        "app.js",
        "no-debugger",
        "function stop() {\n    debugger;\n}\n",
        "function stop() {\n    /* poly: allow[no-debugger] deliberate breakpoint in the debug build */\n    debugger;\n}\n",
    );
}

/// `allow-file` covers the whole file wherever it sits — here below both of the
/// findings it suppresses.
#[test]
fn allow_file_covers_findings_above_the_directive() {
    assert_suppresses(
        "app.js",
        "no-debugger",
        "debugger;\ndebugger;\n",
        "debugger;\ndebugger;\n// poly: allow-file[no-debugger] debug-only entrypoint, excluded from the bundle\n",
    );
}

/// A rule the directive does not name keeps firing: suppression is per-rule, not
/// a blanket line mute.
#[test]
fn an_unnamed_rule_on_the_same_line_still_fires() {
    let codes = codes_for(
        "app.js",
        "debugger; console.log(1); // poly: allow[no-debugger] deliberate breakpoint\n",
    );
    assert!(
        !codes.iter().any(|c| c == "no-debugger"),
        "the named rule is suppressed; got {codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "no-console"),
        "an unnamed rule must survive; got {codes:?}"
    );
}

/// A directive-shaped string literal is not a directive: the character before
/// the marker is a quote, which is deliberately not a comment opener.
#[test]
fn a_string_literal_does_not_suppress() {
    let codes = codes_for("app.js", "const s = \"poly: allow[no-debugger] nope\";\ndebugger;\n");
    assert!(
        codes.iter().any(|c| c == "no-debugger"),
        "a string literal must not suppress; got {codes:?}"
    );
}
