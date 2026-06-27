//! End-to-end configuration tests: schema parsing, file discovery, per-engine
//! slicing, the opinionated-default layering, and proof that user config
//! actually changes engine behavior (ruff rule `ignore`).

use std::fs;

use polylint_core::config::{Config, Kind};
use polylint_core::engine::{Engine, SourceFile};
use polylint_core::engines::ruff::RuffEngine;
use polylint_core::language::Language;

/// With no config file present, the opinionated defaults apply (line length 120).
#[test]
fn default_config_uses_opinionated_defaults() {
    let config = Config::default();
    assert_eq!(config.defaults.line_length, 120);
    assert!(config.defaults.final_newline);
}

/// `[defaults]` in the file overrides the opinionated defaults.
#[test]
fn load_file_overrides_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polylint.toml");
    fs::write(
        &path,
        "[defaults]\nline_length = 100\nfinal_newline = false\n",
    )
    .unwrap();

    let config = Config::load_file(&path).expect("load");
    assert_eq!(config.defaults.line_length, 100);
    assert!(!config.defaults.final_newline);
}

/// `Config::load` walks upward from a nested directory to find `polylint.toml`.
#[test]
fn load_walks_up_to_find_config() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("polylint.toml"),
        "[defaults]\nline_length = 77\n",
    )
    .unwrap();
    let nested = dir.path().join("a/b/c");
    fs::create_dir_all(&nested).unwrap();

    let config = Config::load(&nested).expect("load");
    assert_eq!(
        config.defaults.line_length, 77,
        "should find the ancestor polylint.toml"
    );
}

/// An absent file yields the default config rather than an error.
#[test]
fn load_missing_config_returns_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::load(dir.path()).expect("load");
    assert_eq!(config.defaults.line_length, 120);
}

/// `[lint.<lang>.<engine>]` tables are sliced into the matching engine config.
#[test]
fn engine_config_slices_the_matching_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polylint.toml");
    fs::write(
        &path,
        "[lint.python.ruff]\nignore = [\"F401\"]\nline_length = 99\n\n[fmt.python.ruff]\nline_length = 88\n",
    )
    .unwrap();
    let config = Config::load_file(&path).expect("load");

    let lint_cfg = config.engine_config(&Language::Python, "ruff", Kind::Lint);
    let ignore = lint_cfg
        .options
        .get("ignore")
        .and_then(|v| v.as_array())
        .expect("ignore array");
    assert_eq!(ignore.len(), 1);
    assert_eq!(ignore[0].as_str(), Some("F401"));

    // The format slice is distinct from the lint slice.
    let fmt_cfg = config.engine_config(&Language::Python, "ruff", Kind::Format);
    assert_eq!(
        fmt_cfg
            .options
            .get("line_length")
            .and_then(|v| v.as_integer()),
        Some(88)
    );

    // An unconfigured engine gets empty options.
    let other = config.engine_config(&Language::Python, "nonexistent", Kind::Lint);
    assert!(other.options.is_empty());
}

/// Behavioral proof: ruff lint flags an unused import (F401) by default, and
/// `[lint.python.ruff] ignore = ["F401"]` suppresses it — config reaches the engine.
#[test]
fn ruff_lint_honors_ignore_config() {
    let engine = RuffEngine;
    let src = SourceFile {
        path: "sample.py".into(),
        language: Language::Python,
        content: "import os\n\nx = 1\n".into(),
    };

    // Default config: F401 fires.
    let default_cfg = Config::default().engine_config(&Language::Python, "ruff", Kind::Lint);
    let default_codes: Vec<String> = engine
        .lint(&src, &default_cfg)
        .expect("lint")
        .into_iter()
        .filter_map(|d| d.code)
        .collect();
    assert!(
        default_codes.iter().any(|c| c == "F401"),
        "expected F401 by default, got {default_codes:?}"
    );

    // With ignore = ["F401"]: F401 is suppressed.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("polylint.toml");
    fs::write(&path, "[lint.python.ruff]\nignore = [\"F401\"]\n").unwrap();
    let cfg = Config::load_file(&path).expect("load").engine_config(
        &Language::Python,
        "ruff",
        Kind::Lint,
    );
    let codes: Vec<String> = engine
        .lint(&src, &cfg)
        .expect("lint")
        .into_iter()
        .filter_map(|d| d.code)
        .collect();
    assert!(
        !codes.iter().any(|c| c == "F401"),
        "F401 should be ignored via config, got {codes:?}"
    );
}
