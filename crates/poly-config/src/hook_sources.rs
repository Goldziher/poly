//! Declarative hook sources from `poly-hooks.toml`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

/// Repository-level hook source configuration filename.
pub const HOOK_SOURCE_CONFIG_NAME: &str = "poly-hooks.toml";

/// How a hook source is obtained.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HookInstallChannel {
    /// Use an existing checkout or executable without downloading it.
    System,
    /// Let poly provision the source into its cache.
    #[default]
    Managed,
}

/// Behavior when a managed source has no explicit or machine-default toolchain.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MissingToolchainPolicy {
    /// Reject the configuration before running hooks.
    #[default]
    Error,
    /// Warn and omit the source.
    Warn,
    /// Silently omit the source.
    Skip,
}

/// One local or pinned Git hook source.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookSource {
    /// Stable source identifier used in the lock file and cache path.
    pub id: String,
    /// Repository-relative local directory. Mutually exclusive with `git`.
    pub path: Option<PathBuf>,
    /// Git repository URL. Mutually exclusive with `path`.
    pub git: Option<String>,
    /// Immutable Git revision required for Git sources.
    pub revision: Option<String>,
    /// Dependency provisioning mode. This is independent of source acquisition:
    /// `path` is always live/local and `git` is always lock/cache-backed.
    #[serde(default)]
    pub channel: HookInstallChannel,
    /// Toolchain name or version required by this source.
    pub toolchain: Option<String>,
    /// Installer argv keyed by user-selectable channel (for example `brew`,
    /// `uv`, `cargo`, or `mise`). The first channel present in the machine
    /// preference list wins.
    #[serde(default)]
    pub installers: BTreeMap<String, Vec<String>>,
}

/// Checked-in hook source declarations.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HookSourceConfig {
    /// Schema version. Version `1` is currently supported.
    pub version: u32,
    /// Hook repositories or local source directories.
    pub sources: Vec<HookSource>,
}

/// Machine-only provisioning preferences read from `[hook_preferences]` in
/// `poly.local.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HookMachinePreferences {
    /// Allowed dependency provisioning modes.
    pub install_preference: Vec<HookInstallChannel>,
    /// Ordered package-manager channels used to select `sources.installers`.
    pub channels: Vec<String>,
    /// Toolchain defaults keyed by ecosystem name.
    pub toolchains: BTreeMap<String, String>,
    /// Missing-toolchain behavior.
    pub missing_toolchain: MissingToolchainPolicy,
}

impl Default for HookMachinePreferences {
    fn default() -> Self {
        Self {
            install_preference: vec![HookInstallChannel::System, HookInstallChannel::Managed],
            channels: Vec::new(),
            toolchains: BTreeMap::new(),
            missing_toolchain: MissingToolchainPolicy::Error,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LocalPreferencesFile {
    hook_preferences: HookMachinePreferences,
}

/// Load and validate hook sources plus machine-local preferences from `root`.
pub fn load_hook_source_config(root: &Path) -> anyhow::Result<Option<(HookSourceConfig, HookMachinePreferences)>> {
    let path = root.join(HOOK_SOURCE_CONFIG_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config: HookSourceConfig = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    validate(&config)?;

    let local = root.join(super::LOCAL_OVERRIDE_NAME);
    let preferences = if local.is_file() {
        let text = std::fs::read_to_string(&local).with_context(|| format!("reading {}", local.display()))?;
        toml::from_str::<LocalPreferencesFile>(&text)
            .with_context(|| format!("parsing hook preferences in {}", local.display()))?
            .hook_preferences
    } else {
        HookMachinePreferences::default()
    };
    validate_preferences(&preferences)?;
    for source in &config.sources {
        if !preferences.install_preference.contains(&source.channel) {
            bail!(
                "hook source {:?} requests dependency mode {:?}, which is disabled by hook_preferences.install_preference",
                source.id,
                source.channel
            );
        }
    }
    Ok(Some((config, preferences)))
}

fn validate_preferences(preferences: &HookMachinePreferences) -> anyhow::Result<()> {
    if preferences.install_preference.is_empty() {
        bail!("hook_preferences.install_preference cannot be empty");
    }
    let mut channels = std::collections::BTreeSet::new();
    for channel in &preferences.install_preference {
        if !channels.insert(*channel as u8) {
            bail!("hook_preferences.install_preference contains a duplicate channel");
        }
    }
    Ok(())
}

fn validate(config: &HookSourceConfig) -> anyhow::Result<()> {
    if config.version != 1 {
        bail!("unsupported poly-hooks.toml version {}; expected 1", config.version);
    }
    let mut ids = std::collections::BTreeSet::new();
    for source in &config.sources {
        if source.id.is_empty()
            || !source
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            bail!(
                "hook source id {:?} must contain only letters, digits, '-' or '_'",
                source.id
            );
        }
        if !ids.insert(&source.id) {
            bail!("duplicate hook source id {:?}", source.id);
        }
        match (&source.path, &source.git) {
            (Some(path), None) => {
                validate_local_path(path).with_context(|| format!("invalid path for hook source {:?}", source.id))?;
                if source.revision.is_some() {
                    bail!("local hook source {:?} cannot set revision", source.id);
                }
            }
            (None, Some(_)) => {
                if source.revision.as_deref().is_none_or(str::is_empty) {
                    bail!("Git hook source {:?} requires a pinned revision", source.id);
                }
            }
            _ => bail!("hook source {:?} must set exactly one of path or git", source.id),
        }
    }
    Ok(())
}

fn validate_local_path(path: &Path) -> anyhow::Result<()> {
    if path.is_absolute() {
        bail!("local hook paths must be repository-relative");
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("local hook paths cannot escape the repository");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_local_and_git_sources_with_machine_preferences() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(HOOK_SOURCE_CONFIG_NAME),
            r#"
version = 1
[[sources]]
id = "local"
path = "hooks/local"
channel = "system"
[[sources]]
id = "remote"
git = "https://example.com/hooks.git"
revision = "0123456789abcdef"
toolchain = "python"
"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join(super::super::LOCAL_OVERRIDE_NAME),
            r#"
[hook_preferences]
install_preference = ["managed", "system"]
missing_toolchain = "warn"
[hook_preferences.toolchains]
python = "3.13"
"#,
        )
        .unwrap();

        let (config, preferences) = load_hook_source_config(root.path()).unwrap().unwrap();
        assert_eq!(config.sources.len(), 2);
        assert_eq!(preferences.toolchains["python"], "3.13");
        assert_eq!(preferences.missing_toolchain, MissingToolchainPolicy::Warn);
    }

    #[test]
    fn rejects_escaping_local_path() {
        let config: HookSourceConfig = toml::from_str(
            r#"
version = 1
[[sources]]
id = "escape"
path = "../outside"
channel = "system"
"#,
        )
        .unwrap();
        assert!(validate(&config).unwrap_err().to_string().contains("escape"));
    }

    #[test]
    fn local_source_may_use_managed_dependencies() {
        let config: HookSourceConfig = toml::from_str(
            r#"
version = 1
[[sources]]
id = "local"
path = "hooks"
channel = "managed"
toolchain = "python"
"#,
        )
        .unwrap();
        validate(&config).unwrap();
    }

    #[test]
    fn generic_install_argv_is_rejected() {
        let result = toml::from_str::<HookSourceConfig>(
            r#"
version = 1
[[sources]]
id = "remote"
git = "https://example.com/hooks"
revision = "main"
install = ["sh", "-c", "unsafe"]
"#,
        );
        assert!(result.is_err());
    }
}
