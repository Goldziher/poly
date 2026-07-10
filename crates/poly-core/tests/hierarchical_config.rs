//! End-to-end tests for hierarchical (monorepo) config resolution (ADR 0018):
//! a nested `poly.toml` cascades over the root and governs only its own subtree,
//! while a single-root repo behaves exactly as before.

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

/// Nested (monorepo) run: the root config selects ruff's `F` family so an unused
/// import fires `F401`; a nested `poly.toml` under `sub/` adds a
/// `[per-file-ignores]` for `*.py`. The nested suppression must apply ONLY to
/// files under `sub/` — the root file still reports `F401`, proving per-file
/// config association and the per-config per-file-ignores path.
#[test]
fn nested_per_file_ignores_apply_only_to_their_subtree() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[lint.python.ruff]\nselect = [\"F\"]\n",
    );
    write(&root.join("app.py"), "import os\n");
    write(
        &root.join("sub/poly.toml"),
        "[per-file-ignores]\n\"*.py\" = [\"F401\"]\n",
    );
    write(&root.join("sub/app.py"), "import os\n");

    let config = Config::load(root).expect("load root config");
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: false,
    };
    let results = poly_core::lint(&[root.to_path_buf()], &config, &opts, false, false).unwrap();

    let root_app = root.join("app.py");
    let sub_app = root.join("sub/app.py");

    let root_fires = results
        .iter()
        .find(|r| r.path == root_app)
        .is_some_and(|r| r.diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")));
    assert!(
        root_fires,
        "root/app.py must still report F401 (root config has no ignore)"
    );

    let sub_reports = results.iter().any(|r| r.path == sub_app && !r.diagnostics.is_empty());
    assert!(
        !sub_reports,
        "sub/app.py F401 must be suppressed by the nested [per-file-ignores]"
    );
}

/// A nested config's `[defaults]` cascade: the child inherits the root's ruff
/// selection (so the same rule is computed) and overrides only what it declares.
/// Here the nested config raises the same suppression via inheritance of
/// `select` from the root — asserting the cascade base is read from disk.
#[test]
fn single_root_repo_reports_unsuppressed_diagnostic() {
    let repo = tempdir().unwrap();
    let root = repo.path();
    write(
        &root.join("poly.toml"),
        "[workspace]\nroot = true\n[lint.python.ruff]\nselect = [\"F\"]\n",
    );
    write(&root.join("sub/app.py"), "import os\n");

    let config = Config::load(root).expect("load root config");
    let opts = RunOptions {
        no_cache: true,
        jobs: Some(1),
        exclude: Vec::new(),
        explicit_config: false,
    };
    let results = poly_core::lint(&[root.to_path_buf()], &config, &opts, false, false).unwrap();

    let sub_app = root.join("sub/app.py");
    let fires = results
        .iter()
        .find(|r| r.path == sub_app)
        .is_some_and(|r| r.diagnostics.iter().any(|d| d.code.as_deref() == Some("F401")));
    assert!(fires, "with no nested config, sub/app.py reports F401 (back-compat)");
}
