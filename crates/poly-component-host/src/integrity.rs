//! Re-verifying a cached component without touching the network.
//!
//! alef's `ArtifactCache::install` treats a cached object that fails verification as absent: it
//! drops the verification error, then downloads again (or reports `Offline` when it cannot). So
//! the manager alone cannot tell "never installed" from "installed and damaged". [`CacheCheck`]
//! is that answer, taken from a second, offline cache over the same root and trust keys. It
//! cannot name which check failed, because alef does not surface it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alef_component_runtime::{ArtifactCache, ComponentError, ComponentLock, TrustPolicy};

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

/// How long to wait before believing a failed verification. alef repairs an object under its
/// `<sha>.lock`, which this check does not take, so a check that lands mid-repair sees a half
/// replaced directory (typically a missing library) that is healthy a moment later.
const REPAIR_SETTLE: Duration = Duration::from_millis(150);

/// Whether `error` means the cached object itself is wrong, as opposed to the check not being
/// able to run (descriptor limits, permissions, a failing disk). Only the former may lead to the
/// object being deleted and downloaded again.
///
/// alef's `install` discards the error of its cached-object check and reports whatever the
/// fallback then hits, so on this offline verifier a failed check usually arrives as
/// [`ComponentError::Offline`] and says nothing about its cause; [`unreadable`] tells those apart.
fn damages_the_object(error: &ComponentError) -> bool {
    use ComponentError as E;
    match error {
        E::Offline { .. }
        | E::DigestMismatch { .. }
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

/// The first I/O failure, other than a missing file, from reading the manifest and the library of
/// the object at `path`: the reads verification performs, repeated to learn whether it failed on
/// the content or on the read.
fn unreadable(path: &Path) -> Option<std::io::Error> {
    let not_content = |error: &std::io::Error| error.kind() != std::io::ErrorKind::NotFound;
    let manifest = match std::fs::read(path.join("component.json")) {
        Ok(bytes) => bytes,
        Err(error) => return not_content(&error).then_some(error),
    };
    let library = serde_json::from_slice::<serde_json::Value>(&manifest)
        .ok()
        .and_then(|json| json.pointer("/library/file")?.as_str().map(PathBuf::from))
        .filter(|file| {
            file.components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
        })?;
    std::fs::read(path.join(library)).err().filter(not_content)
}

impl ComponentHost {
    /// Re-verify the cached copy of `component_id` against the lock. Never downloads and never
    /// repairs.
    ///
    /// A failed verification is repeated once after a short wait, so a repair running in another
    /// process does not read as damage.
    ///
    /// # Errors
    ///
    /// When the lock has no entry for `component_id` on this target, or the check itself could not
    /// run (an I/O error other than a missing file): the copy is then of unknown state and must
    /// not be replaced.
    pub fn check_cache(&self, component_id: &str) -> anyhow::Result<CacheCheck> {
        let first = self.check_cache_once(component_id)?;
        if matches!(first, CacheCheck::Corrupt(_)) {
            std::thread::sleep(REPAIR_SETTLE);
            return self.check_cache_once(component_id);
        }
        Ok(first)
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
        match self.verifier.install(entry) {
            Ok(_) => Ok(CacheCheck::Intact),
            Err(error) if damages_the_object(&error) => match unreadable(&path) {
                Some(io) => Err(anyhow::Error::new(io).context(format!(
                    "could not read the cached copy of component `{component_id}` at {}",
                    path.display()
                ))),
                None => Ok(CacheCheck::Corrupt(format!(
                    "the cached copy of component `{component_id}` at {} failed verification \
                 (a digest, size, manifest or signature check)",
                    path.display()
                ))),
            },
            Err(error) => Err(anyhow::Error::new(error).context(format!(
                "could not verify the cached copy of component `{component_id}` at {}",
                path.display()
            ))),
        }
    }
}
