//! The `[[tools]]` half of `poly.lock`: the release of each downloadable tool a
//! repository pinned, with the URL and SHA-256 of every platform's asset.
//!
//! Every entry point that builds a run reads `poly.lock` and [`install`]s the
//! entries once per process (`poly-cli` through its config resolver,
//! `poly-host-services`, behind `poly mcp`, through [`install_from_repo`]);
//! the tool provisioner in `poly-core` asks [`get`] before it resolves anything,
//! so a locked tool needs no network round trip to know what to download.

use std::path::Path;
use std::sync::RwLock;

use anyhow::Context;

use serde::{Deserialize, Serialize};

/// One platform's download of a locked tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedAsset {
    /// `<os>-<arch>` (`linux-x86_64`), or `any` for a platform-independent file.
    pub platform: String,
    /// Download URL.
    pub url: String,
    /// Lower-case hex SHA-256 the download must have.
    pub sha256: String,
    /// Path of the executable inside an archive asset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

/// A downloadable tool pinned to a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedTool {
    /// Catalog name (`golangci-lint`).
    pub name: String,
    /// The pinned release version (`2.15.0`).
    pub version: String,
    /// The release's assets, one per platform.
    pub assets: Vec<LockedAsset>,
}

impl LockedTool {
    /// The asset serving `platform`, falling back to the platform-neutral `any`.
    pub fn asset(&self, platform: &str) -> Option<&LockedAsset> {
        self.assets
            .iter()
            .find(|asset| asset.platform == platform)
            .or_else(|| self.assets.iter().find(|asset| asset.platform == "any"))
    }
}

static LOCKED: RwLock<Vec<LockedTool>> = RwLock::new(Vec::new());

/// Make `tools` the process's locked tools, replacing any earlier set.
pub fn install(tools: Vec<LockedTool>) {
    *LOCKED.write().unwrap_or_else(std::sync::PoisonError::into_inner) = tools;
}

/// Install the `[[tools]]` of the `poly.lock` at the root of the repository
/// holding `start` (the nearest ancestor with a `.git`, else `start` itself),
/// replacing any earlier set; no `poly.lock` installs an empty set.
///
/// This reads only the tool entries; `poly-cli` owns validating the rest of the
/// file.
///
/// # Errors
///
/// Returns `Err` when `poly.lock` exists but cannot be read or parsed.
pub fn install_from_repo(start: &Path) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct ToolsOnly {
        #[serde(default)]
        tools: Vec<LockedTool>,
    }
    let start = if start.is_file() {
        start.parent().unwrap_or(start)
    } else {
        start
    };
    let root = start.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(start);
    let path = root.join("poly.lock");
    if !path.is_file() {
        install(Vec::new());
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: ToolsOnly = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    install(parsed.tools);
    Ok(())
}

/// The locked entry for the tool called `name`, if `poly.lock` pins it.
pub fn get(name: &str) -> Option<LockedTool> {
    LOCKED
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|tool| tool.name == name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests of the process-global lock and restores it on drop.
    pub(crate) struct Guard {
        _held: std::sync::MutexGuard<'static, ()>,
        before: Vec<LockedTool>,
    }

    pub(crate) fn guard() -> Guard {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let held = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = LOCKED.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        Guard { _held: held, before }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            install(std::mem::take(&mut self.before));
        }
    }

    #[test]
    fn the_lock_beside_a_repository_is_installed_from_any_directory_in_it() {
        let _guard = guard();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let nested = repo.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            repo.path().join("poly.lock"),
            "version = 1\n[[tools]]\nname = \"hadolint\"\nversion = \"9.9.9\"\n\
             [[tools.assets]]\nplatform = \"any\"\nurl = \"https://example.invalid/h\"\nsha256 = \"aa\"\n",
        )
        .unwrap();
        install(Vec::new());
        install_from_repo(&nested).unwrap();
        assert_eq!(get("hadolint").unwrap().version, "9.9.9");
        std::fs::remove_file(repo.path().join("poly.lock")).unwrap();
        install_from_repo(&nested).unwrap();
        assert!(get("hadolint").is_none(), "no lock means no pins");
    }

    fn tool(platforms: &[&str]) -> LockedTool {
        LockedTool {
            name: "t".into(),
            version: "1.0.0".into(),
            assets: platforms
                .iter()
                .map(|platform| LockedAsset {
                    platform: (*platform).into(),
                    url: format!("https://example.invalid/{platform}"),
                    sha256: "0".repeat(64),
                    member: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_platform_falls_back_to_the_neutral_asset() {
        let locked = tool(&["linux-x86_64", "any"]);
        assert_eq!(locked.asset("linux-x86_64").unwrap().platform, "linux-x86_64");
        assert_eq!(locked.asset("darwin-aarch64").unwrap().platform, "any");
        assert!(tool(&["linux-x86_64"]).asset("darwin-aarch64").is_none());
    }
}
