//! Release sources: where a downloadable tool's releases come from and how its
//! assets are named, so poly can resolve *any* upstream version instead of only
//! the ones pinned in the overlay (ADR 0047).
//!
//! A [`Source`] is data, not code. It names the host (a GitHub repository or a
//! plain URL), a template for each platform's asset, and where the SHA-256 of an
//! asset is published. Everything network-shaped (the API calls, retries,
//! caching) lives in `poly-core`; this crate only describes and validates.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::ReleaseKind;

/// The placeholder every template substitutes with the release version.
pub const VERSION_PLACEHOLDER: &str = "{version}";

/// The placeholder of a checksum-sidecar template for the asset's download URL.
pub const URL_PLACEHOLDER: &str = "{url}";

/// Where a tool's releases are published and how its assets are named.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// How a download is unpacked and run.
    pub kind: ReleaseKind,
    /// `owner/repo` on GitHub. Releases come from the GitHub API and assets are
    /// downloaded from the release. Exclusive with [`Source::base_url`].
    #[serde(default)]
    pub github: Option<String>,
    /// Git tag template of a GitHub release (`v{version}`, `checkstyle-{version}`).
    /// Defaults to `v{version}`.
    #[serde(default)]
    pub tag: Option<String>,
    /// Directory URL template assets are downloaded from (`https://get.helm.sh`).
    /// Exclusive with [`Source::github`]; requires [`Source::latest`] and
    /// [`Source::checksum`], since no API reports either.
    #[serde(default)]
    pub base_url: Option<String>,
    /// How the newest version of a [`Source::base_url`] source is found.
    #[serde(default)]
    pub latest: Option<Latest>,
    /// Where asset digests are published. For a GitHub source this is the
    /// fallback for releases whose API listing carries no `digest`.
    #[serde(default)]
    pub checksum: Option<Checksum>,
    /// One asset per `<os>-<arch>` platform (`darwin-aarch64`, `linux-x86_64`);
    /// the key `any` serves every platform.
    pub assets: BTreeMap<String, AssetSpec>,
}

/// The asset of one platform, as templates over the release version.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSpec {
    /// File name of the asset (`golangci-lint-{version}-linux-amd64.tar.gz`).
    pub name: String,
    /// Path of the executable inside a `.tar.gz` / `.tgz` / `.zip` asset.
    #[serde(default)]
    pub member: Option<String>,
}

/// How the newest version of a non-GitHub source is found.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Latest {
    /// `owner/repo` whose latest GitHub release names the version (the host
    /// itself has no API, e.g. get.helm.sh).
    #[serde(default)]
    pub github: Option<String>,
    /// Tag template of that repository's releases; defaults to `v{version}`.
    #[serde(default)]
    pub tag: Option<String>,
    /// URL of a Maven `maven-metadata.xml`; its `<release>` is the version.
    #[serde(default)]
    pub maven_metadata: Option<String>,
}

/// Where the SHA-256 of an asset is published, when the host's API does not say.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checksum {
    /// URL template of a per-asset file whose first word is the digest
    /// (`{url}.sha256`, `{url}.sha256sum`). `{url}` is the asset's download URL.
    #[serde(default)]
    pub sidecar: Option<String>,
    /// Name template of a file next to the assets that lists `<digest>  <name>`
    /// lines (`checksums.txt`, `tofu_{version}_SHA256SUMS`).
    #[serde(default)]
    pub list: Option<String>,
}

fn fill(template: &str, version: &str) -> String {
    template.replace(VERSION_PLACEHOLDER, version)
}

impl Source {
    /// The tag template, `v{version}` unless the source sets one.
    pub fn tag_template(&self) -> &str {
        self.tag.as_deref().unwrap_or("v{version}")
    }

    /// The Git tag of `version` (GitHub sources).
    pub fn tag_of(&self, version: &str) -> String {
        fill(self.tag_template(), version)
    }

    /// The directory URL assets of `version` are downloaded from.
    pub fn download_base(&self, version: &str) -> Option<String> {
        if let Some(repo) = &self.github {
            return Some(format!(
                "https://github.com/{repo}/releases/download/{}",
                self.tag_of(version)
            ));
        }
        self.base_url
            .as_deref()
            .map(|base| fill(base, version).trim_end_matches('/').to_owned())
    }

    /// The asset of `platform` (falling back to `any`).
    pub fn asset_spec(&self, platform: &str) -> Option<&AssetSpec> {
        self.assets.get(platform).or_else(|| self.assets.get("any"))
    }

    /// The file name of `spec` at `version`.
    pub fn asset_name(spec: &AssetSpec, version: &str) -> String {
        fill(&spec.name, version)
    }

    /// The archive member of `spec` at `version`.
    pub fn member(spec: &AssetSpec, version: &str) -> Option<String> {
        spec.member.as_deref().map(|member| fill(member, version))
    }

    /// The download URL of the asset called `name` at `version`.
    pub fn asset_url(&self, version: &str, name: &str) -> Option<String> {
        self.download_base(version).map(|base| format!("{base}/{name}"))
    }

    /// The version a release tag of this source stands for, or `None` when the
    /// tag does not fit the tag template.
    pub fn version_of_tag(&self, tag: &str) -> Option<String> {
        version_of(self.latest_tag_template(), tag)
    }

    /// The tag template that [`Latest::github`] releases follow, falling back to
    /// the source's own.
    pub fn latest_tag_template(&self) -> &str {
        self.latest
            .as_ref()
            .and_then(|latest| latest.tag.as_deref())
            .unwrap_or_else(|| self.tag_template())
    }

    /// Check the source is internally consistent.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first inconsistency: both or neither host,
    /// a URL source with no way to find the latest version or a digest, a
    /// template that cannot yield a version, or an empty asset table.
    pub fn validate(&self) -> Result<()> {
        if self.assets.is_empty() {
            bail!("a source needs at least one asset");
        }
        match (&self.github, &self.base_url) {
            (Some(repo), None) => {
                if repo.split('/').filter(|part| !part.is_empty()).count() != 2 {
                    bail!("`github` must be `owner/repo`, got `{repo}`");
                }
                if !self.tag_template().contains(VERSION_PLACEHOLDER) {
                    bail!("the tag template must contain {VERSION_PLACEHOLDER}");
                }
                if self.latest.is_some() {
                    bail!("`latest` is only for `base_url` sources; a GitHub source asks the API");
                }
            }
            (None, Some(base)) => {
                if !base.starts_with("https://") && !base.starts_with("http://") {
                    bail!("`base_url` must be an http(s) URL, got `{base}`");
                }
                let Some(latest) = &self.latest else {
                    bail!("a `base_url` source needs `latest`: no API reports its newest version");
                };
                latest.validate()?;
                if self.checksum.is_none() {
                    bail!("a `base_url` source needs `checksum`: no API reports an asset digest");
                }
            }
            _ => bail!("a source needs exactly one of `github` and `base_url`"),
        }
        if let Some(checksum) = &self.checksum {
            checksum.validate()?;
        }
        for (platform, spec) in &self.assets {
            if spec.name.is_empty() || spec.name.contains('/') {
                bail!("asset `{platform}` needs a plain file name, got `{}`", spec.name);
            }
        }
        Ok(())
    }
}

impl Latest {
    fn validate(&self) -> Result<()> {
        match (&self.github, &self.maven_metadata) {
            (Some(repo), None) => {
                if repo.split('/').filter(|part| !part.is_empty()).count() != 2 {
                    bail!("`latest.github` must be `owner/repo`, got `{repo}`");
                }
                if let Some(tag) = &self.tag
                    && !tag.contains(VERSION_PLACEHOLDER)
                {
                    bail!("`latest.tag` must contain {VERSION_PLACEHOLDER}");
                }
                Ok(())
            }
            (None, Some(url)) if url.starts_with("http") => Ok(()),
            _ => bail!("`latest` needs exactly one of `github` and an http(s) `maven_metadata`"),
        }
    }
}

impl Checksum {
    fn validate(&self) -> Result<()> {
        match (&self.sidecar, &self.list) {
            (Some(template), None) if template.contains(URL_PLACEHOLDER) => Ok(()),
            (Some(_), None) => bail!("`checksum.sidecar` must contain {URL_PLACEHOLDER}"),
            (None, Some(name)) if !name.is_empty() && !name.contains('/') => Ok(()),
            _ => bail!("`checksum` needs exactly one of `sidecar` and `list`"),
        }
    }

    /// The sidecar URL for an asset downloaded from `url`.
    pub fn sidecar_url(&self, url: &str) -> Option<String> {
        self.sidecar
            .as_deref()
            .map(|template| template.replace(URL_PLACEHOLDER, url))
    }

    /// The list file name at `version`.
    pub fn list_name(&self, version: &str) -> Option<String> {
        self.list.as_deref().map(|name| fill(name, version))
    }
}

/// Strip the static prefix and suffix of `template` (the text around
/// `{version}`) from `tag`.
fn version_of(template: &str, tag: &str) -> Option<String> {
    let (prefix, suffix) = template.split_once(VERSION_PLACEHOLDER)?;
    let middle = tag.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!middle.is_empty()).then(|| middle.to_owned())
}

/// The version a tag stands for under `template`; see [`Source::version_of_tag`].
pub fn version_from_tag(template: &str, tag: &str) -> Option<String> {
    version_of(template, tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(json: &str) -> Source {
        serde_json::from_str(json).expect("source")
    }

    const GOLANGCI: &str = r#"{"kind":"binary","github":"golangci/golangci-lint","assets":{
        "linux-x86_64":{"name":"golangci-lint-{version}-linux-amd64.tar.gz",
                        "member":"golangci-lint-{version}-linux-amd64/golangci-lint"}}}"#;

    #[test]
    fn a_github_source_builds_tags_and_urls_from_the_version() {
        let source = github(GOLANGCI);
        source.validate().expect("valid");
        assert_eq!(source.tag_of("2.15.0"), "v2.15.0");
        let spec = source.asset_spec("linux-x86_64").expect("asset");
        let name = Source::asset_name(spec, "2.15.0");
        assert_eq!(name, "golangci-lint-2.15.0-linux-amd64.tar.gz");
        assert_eq!(
            source.asset_url("2.15.0", &name).as_deref(),
            Some(
                "https://github.com/golangci/golangci-lint/releases/download/v2.15.0/golangci-lint-2.15.0-linux-amd64.tar.gz"
            )
        );
        assert_eq!(
            Source::member(spec, "2.15.0").as_deref(),
            Some("golangci-lint-2.15.0-linux-amd64/golangci-lint")
        );
    }

    #[test]
    fn a_tag_maps_back_to_its_version() {
        assert_eq!(version_from_tag("v{version}", "v2.15.0").as_deref(), Some("2.15.0"));
        assert_eq!(
            version_from_tag("checkstyle-{version}", "checkstyle-14.4.0").as_deref(),
            Some("14.4.0")
        );
        assert_eq!(version_from_tag("v{version}", "nightly"), None);
        assert_eq!(version_from_tag("v{version}", "v"), None);
    }

    #[test]
    fn a_url_source_needs_a_latest_and_a_checksum() {
        let bare = github(r#"{"kind":"binary","base_url":"https://get.helm.sh","assets":{"any":{"name":"helm"}}}"#);
        let error = bare.validate().expect_err("no latest").to_string();
        assert!(error.contains("latest"), "{error}");
        let no_checksum = github(
            r#"{"kind":"binary","base_url":"https://get.helm.sh","latest":{"github":"helm/helm"},
                "assets":{"any":{"name":"helm"}}}"#,
        );
        assert!(
            no_checksum
                .validate()
                .expect_err("no checksum")
                .to_string()
                .contains("checksum")
        );
        let full = github(
            r#"{"kind":"binary","base_url":"https://get.helm.sh","latest":{"github":"helm/helm"},
                "checksum":{"sidecar":"{url}.sha256sum"},"assets":{"any":{"name":"helm-v{version}.tar.gz"}}}"#,
        );
        full.validate().expect("valid");
        assert_eq!(
            full.checksum
                .as_ref()
                .and_then(|c| c.sidecar_url("https://x/a.tgz"))
                .as_deref(),
            Some("https://x/a.tgz.sha256sum")
        );
    }

    #[test]
    fn inconsistent_sources_are_rejected() {
        for (json, needle) in [
            (r#"{"kind":"binary","assets":{"any":{"name":"x"}}}"#, "exactly one"),
            (
                r#"{"kind":"binary","github":"a/b","base_url":"https://x","assets":{"any":{"name":"x"}}}"#,
                "exactly one",
            ),
            (
                r#"{"kind":"binary","github":"nope","assets":{"any":{"name":"x"}}}"#,
                "owner/repo",
            ),
            (
                r#"{"kind":"binary","github":"a/b","tag":"fixed","assets":{"any":{"name":"x"}}}"#,
                "tag template",
            ),
            (r#"{"kind":"binary","github":"a/b","assets":{}}"#, "at least one asset"),
            (
                r#"{"kind":"binary","github":"a/b","assets":{"any":{"name":"d/x"}}}"#,
                "plain file name",
            ),
            (
                r#"{"kind":"binary","github":"a/b","checksum":{"sidecar":"x.sha"},"assets":{"any":{"name":"x"}}}"#,
                "{url}",
            ),
        ] {
            let error = github(json).validate().expect_err(json).to_string();
            assert!(error.contains(needle), "{json}: {error}");
        }
    }
}
