//! End-to-end per-rule severity remap: a configured `[lint.<lang>.<tool>.rules
//! .<code>] level` overrides the reported severity of a diagnostic carrying that
//! code, applied uniformly by the runner (not by the engine's own config).

use std::fs;
use std::path::Path;

use polylint_core::{Config, RunOptions, Severity, lint};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// `[lint.python.ruff.rules."F401"] level = <level>`.
fn config_with_f401_level(level: &str) -> Config {
    let mut level_table = toml::Table::new();
    level_table.insert("level".to_string(), toml::Value::String(level.to_string()));
    let mut rules = toml::Table::new();
    rules.insert("F401".to_string(), toml::Value::Table(level_table));
    let mut ruff = toml::Table::new();
    ruff.insert("rules".to_string(), toml::Value::Table(rules));
    let mut python = toml::Table::new();
    python.insert("ruff".to_string(), toml::Value::Table(ruff));
    let mut lint_tables = toml::Table::new();
    lint_tables.insert("python".to_string(), toml::Value::Table(python));
    Config {
        lint: lint_tables,
        ..Config::default()
    }
}

fn opts() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: true,
    }
}

/// `import os` with no use is ruff F401. Configuring `level = "hint"` for F401
/// must lower the reported severity to `Hint` — a value ruff itself never emits
/// (it reports Info/Warning/Error), so seeing it proves the runner applied the
/// post-lint remap.
#[test]
fn per_rule_level_overrides_reported_severity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("unused.py");
    write(&file, "import os\n");

    let config = config_with_f401_level("hint");
    // fix = false: report only, do not rewrite the file.
    let results = lint(&[root.to_path_buf()], &config, &opts(), false, false).unwrap();

    let f401: Vec<&Severity> = results
        .iter()
        .flat_map(|r| r.diagnostics.iter())
        .filter(|d| d.code.as_deref() == Some("F401"))
        .map(|d| &d.severity)
        .collect();

    assert_eq!(f401.len(), 1, "exactly one F401 diagnostic expected");
    assert_eq!(
        *f401[0],
        Severity::Hint,
        "the configured level must override the reported severity"
    );
}
