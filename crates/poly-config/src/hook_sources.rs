//! External hook sources declared under `[[hooks.sources]]` in `poly.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// One local or pinned Git hook catalog.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookSource {
    /// Stable source identifier used in the lock file and cache path.
    pub id: String,
    /// Local catalog directory, mutually exclusive with `git`.
    pub path: Option<PathBuf>,
    /// Git repository URL, mutually exclusive with `path`.
    pub git: Option<String>,
    /// Git ref resolved by `poly hooks update`.
    pub revision: Option<String>,
    /// Explicit producer hook identifiers to enable.
    pub hooks: Vec<String>,
}

/// Machine-only preferences from `[hook_preferences]` in `poly.local.toml`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HookMachinePreferences {
    /// Ordered producer execution channels, for example `npx`, `uvx`, `system`.
    pub channels: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LocalPreferencesFile {
    hook_preferences: HookMachinePreferences,
}

/// Load and validate machine-local hook preferences.
pub fn load_hook_preferences(root: &Path, has_sources: bool) -> anyhow::Result<HookMachinePreferences> {
    let path = root.join(super::LOCAL_OVERRIDE_NAME);
    let preferences = if path.is_file() {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str::<LocalPreferencesFile>(&text)
            .with_context(|| format!("parsing hook preferences in {}", path.display()))?
            .hook_preferences
    } else {
        HookMachinePreferences::default()
    };
    if has_sources && preferences.channels.is_empty() {
        bail!("external hook sources require nonempty hook_preferences.channels in poly.local.toml");
    }
    let mut seen = std::collections::BTreeSet::new();
    for channel in &preferences.channels {
        if channel.is_empty() || !seen.insert(channel) {
            bail!("hook_preferences.channels must contain unique, nonempty channel names");
        }
    }
    Ok(preferences)
}

/// Validate source declarations after parsing `[hooks]`.
pub(crate) fn validate_sources(sources: &[HookSource]) -> Result<(), String> {
    let mut ids = std::collections::BTreeSet::new();
    for source in sources {
        if source.id.is_empty()
            || !source
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(format!(
                "hook source id {:?} must contain only letters, digits, '-' or '_'",
                source.id
            ));
        }
        if !ids.insert(&source.id) {
            return Err(format!("duplicate hook source id {:?}", source.id));
        }
        match (&source.path, &source.git) {
            (Some(_), None) if source.revision.is_none() => {}
            (Some(_), None) => return Err(format!("local hook source {:?} cannot set revision", source.id)),
            (None, Some(_)) if source.revision.as_deref().is_some_and(|revision| !revision.is_empty()) => {}
            (None, Some(_)) => return Err(format!("Git hook source {:?} requires revision", source.id)),
            _ => {
                return Err(format!(
                    "hook source {:?} must set exactly one of path or git",
                    source.id
                ));
            }
        }
        if source.hooks.is_empty() {
            return Err(format!("hook source {:?} must select at least one hook", source.id));
        }
        let unique: std::collections::BTreeSet<_> = source.hooks.iter().collect();
        if unique.len() != source.hooks.len() || source.hooks.iter().any(String::is_empty) {
            return Err(format!(
                "hook source {:?} must select unique, nonempty hook ids",
                source.id
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> HookSource {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn accepts_parent_relative_and_absolute_local_paths() {
        for path in ["../ai-rulez", "/opt/hooks"] {
            validate_sources(&[source(&format!("id='rules'\npath={path:?}\nhooks=['validate']"))]).unwrap();
        }
    }

    #[test]
    fn rejects_git_source_without_revision() {
        let error = validate_sources(&[source(
            "id='rules'\ngit='https://example.com/rules'\nhooks=['validate']",
        )])
        .unwrap_err();
        assert!(error.contains("requires revision"));
    }
}
