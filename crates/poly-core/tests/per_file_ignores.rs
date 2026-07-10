//! End-to-end per-file-ignores: a glob-matched rule is suppressed from the
//! report AND skipped by `--fix`, while non-matching files are unaffected.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use poly_core::{Config, RunOptions, lint};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn config_ignoring(glob: &str, rules: &[&str]) -> Config {
    let mut per_file_ignores = BTreeMap::new();
    per_file_ignores.insert(glob.to_string(), rules.iter().map(|r| r.to_string()).collect());
    Config {
        per_file_ignores,
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

/// `import os` with no use is ruff F401 (unused import), and the autofix removes
/// the line. A per-file-ignore for that path must suppress the diagnostic AND
/// leave the file untouched under `--fix`.
#[test]
fn fix_does_not_rewrite_a_per_file_ignored_rule() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let ignored = root.join("tests/unused.py");
    let active = root.join("src/unused.py");
    write(&ignored, "import os\n");
    write(&active, "import os\n");

    let config = config_ignoring("tests/**", &["F401"]);
    let results = lint(&[root.to_path_buf()], &config, &opts(), true, false).unwrap();

    assert_eq!(
        fs::read_to_string(&ignored).unwrap(),
        "import os\n",
        "a per-file-ignored rule must not be auto-fixed"
    );
    assert!(
        !results.iter().any(|r| r.path == ignored),
        "the ignored file produces no reported diagnostics"
    );

    assert_ne!(
        fs::read_to_string(&active).unwrap(),
        "import os\n",
        "a non-ignored file is still auto-fixed"
    );
}
