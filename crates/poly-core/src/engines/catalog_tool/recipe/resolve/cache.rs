//! On-disk state of release resolution, under `<cache>/tools/<tool>/`:
//!
//! - `latest.json`: the newest release as last asked for, with the validators
//!   (`ETag`, `Last-Modified`) a refresh revalidates with;
//! - `<version>/release.json`: one version's digests, immutable once fetched;
//! - `.resolve.lock`: held while resolving, so parallel poly processes neither
//!   race on the files nor ask the same question twice.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use poly_catalog::Asset;
use serde::{Deserialize, Serialize};

/// How long a resolved `latest` is trusted before it is revalidated.
pub(in crate::engines::catalog_tool) const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

/// A resolved release: its version and the asset of each platform.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(in crate::engines::catalog_tool) struct Release {
    pub(in crate::engines::catalog_tool) version: String,
    #[serde(default)]
    pub(in crate::engines::catalog_tool) assets: BTreeMap<String, ResolvedAsset>,
    /// Unix time the answer was last confirmed with upstream.
    #[serde(default)]
    pub(in crate::engines::catalog_tool) checked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::engines::catalog_tool) etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::engines::catalog_tool) last_modified: Option<String>,
}

/// One platform's download and the digest it must have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::engines::catalog_tool) struct ResolvedAsset {
    pub(in crate::engines::catalog_tool) url: String,
    pub(in crate::engines::catalog_tool) sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::engines::catalog_tool) member: Option<String>,
}

impl ResolvedAsset {
    pub(in crate::engines::catalog_tool) fn to_asset(&self) -> Asset {
        Asset {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            member: self.member.clone(),
        }
    }
}

impl Release {
    /// Whether every one of `keys` has an asset.
    pub(super) fn covers(&self, keys: &[String]) -> bool {
        keys.iter().all(|key| self.assets.contains_key(key))
    }
}

pub(super) fn tool_dir(root: &Path, tool: &str) -> PathBuf {
    root.join(tool)
}

pub(super) fn latest_path(root: &Path, tool: &str) -> PathBuf {
    tool_dir(root, tool).join("latest.json")
}

pub(super) fn version_path(root: &Path, tool: &str, version: &str) -> PathBuf {
    tool_dir(root, tool).join(version).join("release.json")
}

/// Read a record, treating a missing or unreadable file as "nothing cached": a
/// corrupt cache costs a refetch, never a failed run.
pub(super) fn read(path: &Path) -> Option<Release> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Atomically replace `path` with `release`.
pub(super) fn write(path: &Path, release: &Release) -> Result<()> {
    let directory = path.parent().context("cache path has no parent")?;
    std::fs::create_dir_all(directory).with_context(|| format!("creating {}", directory.display()))?;
    let mut staged = tempfile::Builder::new()
        .prefix(".release-")
        .tempfile_in(directory)
        .with_context(|| format!("creating a cache file in {}", directory.display()))?;
    let text = serde_json::to_string_pretty(release).context("serializing the resolved release")?;
    staged
        .write_all(text.as_bytes())
        .context("writing the resolved release")?;
    staged
        .persist(path)
        .with_context(|| format!("installing {}", path.display()))?;
    Ok(())
}

/// An exclusive lock on a file, released on drop. Works across threads and
/// processes alike: each holder has its own open file description.
pub(in crate::engines::catalog_tool) struct FileLock(File);

impl FileLock {
    /// Block until the lock at `path` (created with its directory) is held.
    pub(in crate::engines::catalog_tool) fn acquire(path: &Path) -> Result<Self> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory).with_context(|| format!("creating {}", directory.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("locking {}", path.display()))?;
        Ok(FileLock(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

/// Parse a duration such as `24h`, `90m`, `30s`, `7d`, or a bare `0`.
///
/// # Errors
///
/// Returns a message naming the accepted forms.
pub(in crate::engines::catalog_tool) fn parse_ttl(text: &str) -> Result<Duration> {
    let text = text.trim();
    let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => text.split_at(at),
        None => (text, "s"),
    };
    let Ok(count) = digits.parse::<u64>() else {
        bail!("`resolve_ttl = \"{text}\"` is not a duration; use a number with s, m, h or d (`24h`, `30m`, `0`)");
    };
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => bail!("`resolve_ttl = \"{text}\"` has unit `{unit}`; use s, m, h or d"),
    };
    Ok(Duration::from_secs(count.saturating_mul(seconds)))
}

/// Say `message` on stderr once per `key` for the life of the process.
pub(super) fn warn_once(key: &str, message: &str) {
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if said.get_or_insert_with(HashSet::new).insert(key.to_owned()) {
        tracing::warn!("{message}");
        eprintln!("poly: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_units() {
        assert_eq!(parse_ttl("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_ttl("90m").unwrap(), Duration::from_secs(5_400));
        assert_eq!(parse_ttl("7d").unwrap(), Duration::from_secs(604_800));
        assert_eq!(parse_ttl("0").unwrap(), Duration::ZERO);
        assert!(parse_ttl("soon").unwrap_err().to_string().contains("resolve_ttl"));
        assert!(parse_ttl("5w").unwrap_err().to_string().contains("unit"));
    }

    #[test]
    fn a_corrupt_record_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latest.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read(&path), None);
        let release = Release {
            version: "1.0.0".into(),
            ..Release::default()
        };
        write(&path, &release).unwrap();
        assert_eq!(read(&path), Some(release));
    }
}
