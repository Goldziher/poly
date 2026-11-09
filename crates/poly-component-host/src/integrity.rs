//! Re-verifying a cached component without touching the network.
//!
//! alef's `ArtifactCache::install` repairs a cached object that fails verification, so the
//! manager alone cannot tell "never installed" from "installed and damaged". [`CacheCheck`] is
//! that answer, taken with `ArtifactCache::verify_cached` on a second cache over the same root
//! and trust keys: it never downloads or removes anything, and names the check that failed.

use std::time::Duration;

use alef_component_runtime::{ArtifactCache, ComponentError, ComponentLock, TrustPolicy};

use crate::host::ComponentHost;

/// What re-verifying the cached copy of a component found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheCheck {
    /// There is no cached copy.
    NotCached,
    /// The cached copy passes the lock's digest, size, manifest and signature checks.
    Intact,
    /// There is a cached copy and it fails verification; the text says where, which check failed,
    /// and how to repair it.
    Corrupt(String),
}

/// A cache over `root` that checks objects exactly as a load would.
pub(crate) fn verifier(lock: &ComponentLock, root: std::path::PathBuf) -> ArtifactCache {
    ArtifactCache::new(root, TrustPolicy::EmbeddedKeys(lock.public_keys.clone())).with_verified_stamp(true)
}

/// How long to wait before believing a failed verification. alef repairs an object under its
/// `<sha>.lock`, which this check does not take, so a check that lands mid-repair sees a half
/// replaced directory (typically a missing library) that is healthy a moment later.
const REPAIR_SETTLE: Duration = Duration::from_millis(150);

/// How many times a failed verification is retried, [`REPAIR_SETTLE`] apart, before it is
/// believed. A single retry lost the race on a loaded CI runner, where a repair can take longer
/// than one settle interval to land.
const REPAIR_RETRIES: usize = 4;

/// Whether `error` means the cached object itself is wrong, as opposed to the check not being
/// able to run (descriptor limits, permissions, a failing disk). Only the former may lead to the
/// object being deleted and downloaded again.
fn damages_the_object(error: &ComponentError) -> bool {
    use ComponentError as E;
    match error {
        E::DigestMismatch { .. }
        | E::SizeMismatch { .. }
        | E::SignatureRequired
        | E::InvalidSignatureEncoding
        | E::SignatureVerification
        | E::UnsupportedSignatureAlgorithm(_)
        | E::UnknownSignatureKey(_)
        | E::SignatureKeyMismatch
        | E::NonCanonicalManifest
        | E::ManifestDigestMismatch
        | E::ManifestIdentityMismatch
        | E::UnsupportedManifest
        | E::IdentityMismatch { .. }
        | E::UnsafeLibraryPath(_)
        | E::MissingLibrary(_)
        | E::Json(_) => true,
        E::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

impl ComponentHost {
    /// Re-verify the cached copy of `component_id` against the lock. Never downloads and never
    /// repairs.
    ///
    /// A failed verification is repeated a few times after a short wait, so a repair running in
    /// another process does not read as damage.
    ///
    /// # Errors
    ///
    /// When the lock has no entry for `component_id` on this target, or the check itself could not
    /// run (an I/O error other than a missing file): the copy is then of unknown state and must
    /// not be replaced.
    pub fn check_cache(&self, component_id: &str) -> anyhow::Result<CacheCheck> {
        let mut check = self.check_cache_once(component_id)?;
        for _ in 0..REPAIR_RETRIES {
            if !matches!(check, CacheCheck::Corrupt(_)) {
                break;
            }
            std::thread::sleep(REPAIR_SETTLE);
            check = self.check_cache_once(component_id)?;
        }
        Ok(check)
    }

    fn check_cache_once(&self, component_id: &str) -> anyhow::Result<CacheCheck> {
        let entry = self.manager.lock_entry(component_id)?;
        let path = self.manager.cache_path(component_id)?;
        match std::fs::metadata(path.join("component.json")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(CacheCheck::NotCached),
            Err(error) => {
                return Err(anyhow::Error::new(error).context(format!(
                    "could not inspect the cached copy of component `{component_id}`"
                )));
            }
        }
        match self.verifier.verify_cached(entry) {
            Ok(_) => Ok(CacheCheck::Intact),
            Err(error) if damages_the_object(&error) => Ok(CacheCheck::Corrupt(format!(
                "the cached copy of component `{component_id}` at {} failed verification: {error}",
                path.display()
            ))),
            Err(error) => {
                let action = if matches!(error, ComponentError::Io(_)) {
                    "read"
                } else {
                    "verify"
                };
                Err(anyhow::Error::new(error).context(format!(
                    "could not {action} the cached copy of component `{component_id}` at {}",
                    path.display()
                )))
            }
        }
    }
}
