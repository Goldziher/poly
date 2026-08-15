//! Shared fixtures for the hook-source unit tests: a producer catalog, the
//! consumer's `[[sources]]` selection, and this machine's channel preferences.

use std::path::Path;

use poly_config::HooksConfig;

use super::manifest::PRODUCER_MANIFEST_NAME;

pub(super) fn write_consumer(_root: &Path, source: &Path, hooks: &[&str]) -> HooksConfig {
    let selected = hooks.iter().map(|id| format!("{id:?}")).collect::<Vec<_>>().join(",");
    toml::from_str(&format!(
        "[[sources]]\nid='rules'\npath={:?}\nhooks=[{}]",
        source.to_string_lossy(),
        selected
    ))
    .unwrap()
}
pub(super) fn write_catalog(root: &Path) {
    std::fs::write(
        root.join(PRODUCER_MANIFEST_NAME),
        r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
args = ["ok"]
workspace = true
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
run = "printf"
[[hooks]]
id = "other"
stages = ["pre-push"]
[[hooks.paths]]
channel = "shell"
check = "false"
run = "false"
"#,
    )
    .unwrap();
}
pub(super) fn preferences(root: &Path) {
    std::fs::write(root.join("poly.local.toml"), "[hook_preferences]\nchannels=['shell']\n").unwrap();
}
