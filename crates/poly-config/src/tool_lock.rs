//! The `[[tools]]` half of `poly.lock`: the release of each downloadable tool a
//! repository pinned, with the URL and SHA-256 of every platform's asset.
//!
//! `poly-cli` reads `poly.lock` and [`install`]s the entries once per process;
//! the tool provisioner in `poly-core` asks [`get`] before it resolves anything,
//! so a locked tool needs no network round trip to know what to download.

use std::sync::RwLock;

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
