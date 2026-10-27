//! Ruff settings are memoized per process, so the memo key must be the *resolved*
//! per-directory config. Two directories with different nested ruff configs, linted
//! in one run (one process, one memo), must each get their own rule set.
#![cfg(unix)]

use std::fs;
use std::process::Command;

const POLY: &str = env!("CARGO_BIN_EXE_poly");

/// An unused import and a line far longer than 20 columns: F401 and E501 both apply.
const SOURCE: &str = "import os\nvalue = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n";

fn nested_config(select: &str, line_length: u32) -> String {
    format!("[lint.python.ruff]\nselect = [\"{select}\"]\nline_length = {line_length}\n")
}

fn lint_json(root: &std::path::Path) -> serde_json::Value {
    let output = Command::new(POLY)
        .args(["lint", "--no-workspace", "--no-cache", "--format", "json", "."])
        .current_dir(root)
        .output()
        .expect("run poly");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout must be JSON ({e}): {stdout}"))
}

#[test]
fn sibling_directories_with_different_nested_ruff_configs_get_different_settings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir(root.join(".git")).expect("mkdir .git");
    fs::write(root.join("poly.toml"), "[workspace]\nroot = true\n").expect("write root config");
    for (name, select) in [("a", "E501"), ("b", "F401")] {
        fs::create_dir(root.join(name)).expect("mkdir");
        fs::write(root.join(name).join("poly.toml"), nested_config(select, 20)).expect("write nested config");
        fs::write(root.join(name).join("x.py"), SOURCE).expect("write source");
    }

    let report = lint_json(root);

    let codes_for = |file: &str| -> Vec<String> {
        report["results"]
            .as_array()
            .expect("results array")
            .iter()
            .filter(|result| result["path"].as_str().is_some_and(|path| path.ends_with(file)))
            .flat_map(|result| result["diagnostics"].as_array().expect("diagnostics").iter())
            .map(|diagnostic| diagnostic["code"].as_str().expect("code").to_owned())
            .collect()
    };
    assert_eq!(codes_for("a/x.py"), ["E501"], "a/ selects only E501: {report}");
    assert_eq!(codes_for("b/x.py"), ["F401"], "b/ selects only F401: {report}");
}
