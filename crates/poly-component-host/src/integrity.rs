//! Re-verifying a cached component without touching the network.
//!
//! alef's `ArtifactCache::install` treats a cached object that fails verification as absent: it
//! drops the verification error, then downloads again (or reports `Offline` when it cannot). So
//! the manager alone cannot tell "never installed" from "installed and damaged". [`CacheCheck`]
//! is that answer, taken from a second, offline cache over the same root and trust keys. It
//! cannot name which check failed, because alef does not surface it.

use alef_component_runtime::{ArtifactCache, ComponentLock, TrustPolicy};

use crate::host::ComponentHost;

/// URL the verifying cache is given for every entry. It never resolves to a `file://` source, so
/// a verification can neither read nor reinstall from a lock that points at local archives.
const NO_SOURCE: &str = "poly-verify:offline";

/// What re-verifying the cached copy of a component found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheCheck {
    /// There is no cached copy.
    NotCached,
    /// The cached copy passes the lock's digest, size, manifest and signature checks.
    Intact,
    /// There is a cached copy and it fails verification; the text says where and how to repair it.
    Corrupt(String),
}

/// An offline cache over `root` that checks objects exactly as a load would.
pub(crate) fn verifier(lock: &ComponentLock, root: std::path::PathBuf) -> ArtifactCache {
    ArtifactCache::new(root, TrustPolicy::EmbeddedKeys(lock.public_keys.clone()))
        .offline(true)
        .with_verified_stamp(true)
        .with_url_override(|_| NO_SOURCE.to_owned())
}

impl ComponentHost {
    /// Re-verify the cached copy of `component_id` against the lock. Never downloads and never
    /// repairs.
    ///
    /// # Errors
    ///
    /// When the lock has no entry for `component_id` on this target.
    pub fn check_cache(&self, component_id: &str) -> anyhow::Result<CacheCheck> {
        let entry = self.manager.lock_entry(component_id)?;
        let path = self.manager.cache_path(component_id)?;
        if !path.join("component.json").is_file() {
            return Ok(CacheCheck::NotCached);
        }
        Ok(match self.verifier.install(entry) {
            Ok(_) => CacheCheck::Intact,
            Err(_) => CacheCheck::Corrupt(format!(
                "the cached copy of component `{component_id}` at {} failed verification \
                 (a digest, size, manifest or signature check)",
                path.display()
            )),
        })
    }
}
