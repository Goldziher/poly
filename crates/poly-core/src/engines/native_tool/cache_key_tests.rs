//! End-to-end proof that editing a native tool's **own** config file
//! invalidates poly's result cache.
//!
//! The bug this locks down: `rustfmt` reads `rustfmt.toml` itself, but that
//! file was invisible to every component of the cache key — a native tool
//! carries no poly-side args, and the input digest covers only the source file.
//! So `poly fmt` → edit `rustfmt.toml` → `poly fmt` served the first run's
//! formatting under the second run's config, silently.
//!
//! ## Why this test re-executes the test binary
//!
//! The config fingerprint lives inside [`Engine::version`], whose string is
//! memoised in a process-wide `OnceLock` (it is called once per file per engine
//! on the runner's hot path and returns a borrow, so it cannot be recomputed
//! per call). One process therefore sees exactly one fingerprint — which is
//! correct for the CLI, where every invocation is a fresh process, but means a
//! single-process test could never observe the invalidation.
//!
//! So the test *is* two processes: it re-runs this same test binary with
//! [`CHILD_PHASE_DIR_ENV`] set, which puts the test into child-phase mode where
//! it performs one real [`crate::format`] run and returns. That models two
//! `poly fmt` invocations against one warm on-disk cache, with no production
//! test hook and no hot-path cost.
//!
//! [`Engine::version`]: crate::engine::Engine::version

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::language::Language;

use super::NativeToolEngine;

/// Set on the re-executed child: the project directory it should format.
/// Presence of this variable is what selects child-phase mode.
const CHILD_PHASE_DIR_ENV: &str = "POLY_NATIVE_TOOL_CACHE_E2E_DIR";

/// Path libtest needs to select the child phase with `--exact`. Kept in sync
/// with the module path and the test function name below; a mismatch makes the
/// child run zero tests, which [`run_format_phase`] asserts against rather than
/// letting the test pass vacuously.
const E2E_TEST_PATH: &str = "engines::native_tool::cache_key_tests::rustfmt_config_edit_invalidates_the_format_cache";

/// A `main.rs` whose only statement is 99 columns wide: untouched at
/// `max_width = 120`, rewrapped at `max_width = 40`.
const SOURCE: &str = "fn main() {\n    let value = some_function_with_a_long_name(alpha, beta, gamma, delta, epsilon, zeta, eta, theta);\n}\n";

/// Lay out a minimal crate: a manifest (so the `--edition` rustfmt receives is
/// resolved the way `cargo fmt` resolves it), one source file, and a
/// `rustfmt.toml` carrying `max_width`.
fn write_project(root: &Path, max_width: u32) {
    fs::create_dir_all(root.join("src")).expect("create src/");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cache-key-e2e\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(root.join("src/main.rs"), SOURCE).expect("write src/main.rs");
    write_rustfmt_config(root, max_width);
}

fn write_rustfmt_config(root: &Path, max_width: u32) {
    fs::write(root.join("rustfmt.toml"), format!("max_width = {max_width}\n")).expect("write rustfmt.toml");
}

/// Re-execute this test binary in child-phase mode: one real `poly fmt --fix`
/// equivalent over `project`, against the cache rooted at `cache_home`.
fn run_format_phase(project: &Path, cache_home: &Path) {
    let exe = std::env::current_exe().expect("current test binary");
    let output = Command::new(exe)
        .args(["--exact", E2E_TEST_PATH, "--test-threads=1", "--nocapture"])
        .env(CHILD_PHASE_DIR_ENV, project)
        .env("POLY_CACHE_HOME", cache_home)
        .output()
        .expect("re-execute the test binary for the format phase");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "format phase failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1 passed"),
        "the child ran no test — E2E_TEST_PATH ({E2E_TEST_PATH}) is stale.\nstdout: {stdout}"
    );
}

/// The child phase: format `project` in place, from inside it.
///
/// The working directory is what anchors both halves of the behaviour under
/// test — `super::tool_config`'s fingerprint search, and (for `rustfmt`
/// specifically) nothing else, since poly re-anchors the rustfmt child at the
/// source file's own directory. A dedicated process makes `set_current_dir`
/// safe here.
fn format_phase(project: &Path) {
    std::env::set_current_dir(project).expect("enter the project directory");
    let config = crate::Config::load(project).expect("load config");
    let options = crate::RunOptions::default();
    crate::format(&[PathBuf::from(project)], &config, &options, true, false).expect("format run");
}

/// Whether the formatted source still holds the call on one line — i.e.
/// `max_width = 120` was in force.
fn is_one_line_call(formatted: &str) -> bool {
    formatted.contains("let value = some_function_with_a_long_name(alpha,")
}

/// Warm the cache under one `rustfmt.toml`, edit it, run again, and require the
/// output to follow the edit.
///
/// **Fails before the fix**: with no config fingerprint in the cache key, the
/// second run's key is byte-identical to the first's, so the cached (wide)
/// formatting is served and the file never changes.
#[test]
fn rustfmt_config_edit_invalidates_the_format_cache() {
    if let Some(project) = std::env::var_os(CHILD_PHASE_DIR_ENV) {
        format_phase(Path::new(&project));
        return;
    }

    if !NativeToolEngine::for_language(Language::Rust).is_available() {
        eprintln!("rustfmt not found on PATH — skipping rustfmt_config_edit_invalidates_the_format_cache");
        return;
    }

    let project = tempfile::tempdir().expect("project tempdir");
    let cache_home = tempfile::tempdir().expect("cache tempdir");
    write_project(project.path(), 120);

    run_format_phase(project.path(), cache_home.path());
    let after_wide = fs::read_to_string(project.path().join("src/main.rs")).expect("read src/main.rs");
    assert!(
        is_one_line_call(&after_wide),
        "max_width = 120 must leave the 99-column call on one line; got:\n{after_wide}"
    );

    write_rustfmt_config(project.path(), 40);

    run_format_phase(project.path(), cache_home.path());
    let after_narrow = fs::read_to_string(project.path().join("src/main.rs")).expect("read src/main.rs");

    assert_ne!(
        after_narrow, after_wide,
        "editing rustfmt.toml must invalidate the cached formatting; \
         the second run served the first run's output under the new config"
    );
    assert!(
        !is_one_line_call(&after_narrow),
        "max_width = 40 must rewrap the call; got:\n{after_narrow}"
    );
}

/// The supplement to the end-to-end test: the same source under two different
/// `rustfmt.toml` contents must produce two different cache keys.
///
/// This asserts the mechanism directly (fingerprint → `version()` → key)
/// without a second process, so it still fails if the fingerprint is dropped
/// from the key even on a host where `rustfmt` is not installed.
#[test]
fn a_config_edit_changes_the_cache_key() {
    use poly_cache::{Namespace, ResultCache};

    use super::spec::RUSTFMT_SPEC;
    use super::tool_config;

    let project = tempfile::tempdir().expect("tempdir");
    let config_path = project.path().join("rustfmt.toml");

    fs::write(&config_path, "max_width = 120\n").expect("write config");
    let wide = tool_config::fingerprint_at(project.path(), RUSTFMT_SPEC.config_files);

    fs::write(&config_path, "max_width = 40\n").expect("rewrite config");
    let narrow = tool_config::fingerprint_at(project.path(), RUSTFMT_SPEC.config_files);

    let args = ResultCache::serialize_args(&toml::Table::new());
    let digest = ResultCache::single_file_digest(SOURCE);
    let key_of = |fingerprint: &str| {
        ResultCache::key_with_args(
            Namespace::Fmt,
            "rustfmt",
            &format!("rustfmt 1.0.0 | ts:1 | cfg:{fingerprint}"),
            &args,
            &digest,
        )
    };

    assert_ne!(wide, narrow, "the two configs must fingerprint differently");
    assert_ne!(
        key_of(&wide),
        key_of(&narrow),
        "a rustfmt.toml edit must change the cache key"
    );
}

/// Only the tools that actually read a config file declare one. Verified
/// empirically against the installed binaries — see `super::tool_config`'s
/// module docs for each invocation and its result.
#[test]
fn only_config_reading_tools_declare_config_files() {
    use super::spec::{
        DARTFMT_SPEC, GLEAMFMT_SPEC, GOFMT_SPEC, JAVA_FMT_SPEC, KTFMT_SPEC, RSTYLER_SPEC, RUSTFMT_SPEC,
        SHELLCHECK_SPEC, SHFMT_SPEC, SWIFT_FORMAT_SPEC, ZIGFMT_SPEC,
    };

    assert_eq!(RUSTFMT_SPEC.config_files, &[".rustfmt.toml", "rustfmt.toml"]);
    assert_eq!(SHELLCHECK_SPEC.config_files, &[".shellcheckrc", "shellcheckrc"]);
    assert_eq!(SWIFT_FORMAT_SPEC.config_files, &[".swift-format"]);

    for spec in [
        &GOFMT_SPEC,
        &ZIGFMT_SPEC,
        // shfmt reads .editorconfig only when given --filename, which poly
        // never passes; adding it would make .editorconfig an input here.
        &SHFMT_SPEC,
        &JAVA_FMT_SPEC,
        &KTFMT_SPEC,
        &RSTYLER_SPEC,
        // dart format honours analysis_options.yaml only for a named file, not
        // over stdin.
        &DARTFMT_SPEC,
        &GLEAMFMT_SPEC,
    ] {
        assert!(
            spec.config_files.is_empty(),
            "{} reads no config file under poly's argv",
            spec.engine_name
        );
    }
}

/// A tool that declares config files must have its fingerprint in `version()`,
/// and one that declares none must not — otherwise the roles that pay nothing
/// would still churn their cache key.
#[test]
fn version_carries_the_fingerprint_only_for_config_reading_tools() {
    use crate::engine::Engine;

    assert!(
        NativeToolEngine::for_language(Language::Rust)
            .version()
            .contains(" | cfg:"),
        "rustfmt's version must carry the config fingerprint"
    );
    assert!(
        NativeToolEngine::shell_lint().version().contains(" | cfg:"),
        "shellcheck's version must carry the config fingerprint"
    );
    assert!(
        !NativeToolEngine::for_language(Language::Go)
            .version()
            .contains(" | cfg:"),
        "gofmt reads no config file, so its version must not carry a fingerprint"
    );
}
