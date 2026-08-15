//! The producer catalog (`poly-hooks.toml`): its schema, validation, and the
//! rejection of a consumer repository that still carries a producer file.

use std::path::Path;

use anyhow::{Context, bail};
use poly_config::{Job, Stage};
use serde::Deserialize;

pub(super) const PRODUCER_MANIFEST_NAME: &str = "poly-hooks.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookPath {
    pub(super) channel: String,
    pub(super) check: String,
    pub(super) run: String,
    #[serde(default)]
    pub(super) install: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestHook {
    pub(super) id: String,
    pub(super) stages: Vec<Stage>,
    pub(super) paths: Vec<HookPath>,
    #[serde(default)]
    pub(super) pass_filenames: Option<bool>,
    #[serde(default)]
    pub(super) always_run: Option<bool>,
    #[serde(flatten)]
    pub(super) job: Job,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProducerManifest {
    version: u32,
    pub(super) hooks: Vec<ManifestHook>,
}

pub(super) fn reject_legacy_consumer_file(root: &Path) -> anyhow::Result<()> {
    let legacy = root.join(PRODUCER_MANIFEST_NAME);
    if !legacy.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&legacy).with_context(|| format!("reading {}", legacy.display()))?;
    let document: toml::Value = toml::from_str(&text).with_context(|| format!("parsing {}", legacy.display()))?;
    if document.get("sources").is_some() {
        bail!(
            "{} is a producer catalog, not consumer configuration; move source declarations to [[hooks.sources]] in poly.toml",
            legacy.display()
        );
    }
    Ok(())
}

pub(super) fn load_manifest(root: &Path) -> anyhow::Result<ProducerManifest> {
    let path = root.join(PRODUCER_MANIFEST_NAME);
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading hook catalog {}", path.display()))?;
    let manifest: ProducerManifest =
        toml::from_str(&text).with_context(|| format!("parsing hook catalog {}", path.display()))?;
    if manifest.version != 1 {
        bail!(
            "hook catalog {} has unsupported version {}; expected 1",
            path.display(),
            manifest.version
        );
    }
    let mut ids = std::collections::BTreeSet::new();
    for hook in &manifest.hooks {
        if hook.id.is_empty() || !ids.insert(&hook.id) {
            bail!(
                "hook catalog {} contains an empty or duplicate hook id {:?}",
                path.display(),
                hook.id
            );
        }
        if hook.stages.is_empty() {
            bail!("catalog hook {:?} must declare at least one stage", hook.id);
        }
        if hook.stages.contains(&Stage::Always) {
            bail!("catalog hook {:?} cannot use the `always` pseudo-stage", hook.id);
        }
        if hook.job.run.is_some() || hook.job.script.is_some() || hook.job.runner.is_some() {
            bail!(
                "catalog hook {:?} must declare execution only through [[hooks.paths]]",
                hook.id
            );
        }
        if hook.paths.is_empty() {
            bail!("catalog hook {:?} must declare at least one execution path", hook.id);
        }
        let mut channels = std::collections::BTreeSet::new();
        for execution in &hook.paths {
            if execution.channel.is_empty() || execution.check.is_empty() || execution.run.is_empty() {
                bail!(
                    "catalog hook {:?} paths require nonempty channel, check, and run",
                    hook.id
                );
            }
            if execution.install.as_ref().is_some_and(String::is_empty) {
                bail!("catalog hook {:?} path install command cannot be empty", hook.id);
            }
            if !channels.insert(&execution.channel) {
                bail!(
                    "catalog hook {:?} has duplicate channel {:?}",
                    hook.id,
                    execution.channel
                );
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_catalog_execution_outside_guarded_paths() {
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "unsafe"
stages = ["pre-commit"]
run = "false"
[[hooks.paths]]
channel = "shell"
check = "true"
run = "true"
"#,
        )
        .unwrap();
        assert!(
            load_manifest(producer.path())
                .unwrap_err()
                .to_string()
                .contains("only through [[hooks.paths]]")
        );
    }
}
