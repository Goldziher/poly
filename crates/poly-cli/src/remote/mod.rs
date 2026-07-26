//! Reusable remote-git mirror + checkout machinery.
//!
//! These primitives back any feature that needs to fetch a pinned revision of a
//! remote repository into a shared, content-addressed, read-only cache — today
//! the `[[hooks.sources]]` provisioner (see [`crate::hooks::sources`]), and a
//! forthcoming config-`extends` resolver.
//!
//! The layout under a per-URL `<source>` directory is:
//!
//! ```text
//! <source>/mirror.git              bare `--mirror` clone (origin URL verified)
//! <source>/checkouts/<oid>/        detached, read-only working tree per revision
//! ```
//!
//! Security invariants preserved end to end: the mirror's `remote.origin.url`
//! must match the configured URL (blocking `insteadOf` substitution), locked
//! revisions must be full hexadecimal object IDs (blocking path traversal),
//! checkouts are read-only, and a tampered or drifted checkout is rebuilt.
//!
//! Callers that need cross-process exclusion around a source directory hold
//! their own advisory lock (the hooks flow does); [`materialize`] itself does
//! not lock, so a caller with concurrent writers must serialize them.

pub mod checkout;
pub mod git;

use std::path::{Path, PathBuf};

use anyhow::Context;

pub use checkout::{checkout_is_valid, make_read_only, make_writable, materialize_checkout};
pub use git::{
    ensure_commit, ensure_mirror, git_object_exists, git_output, run_command, run_git, validate_locked_revision,
};

/// Materialize a read-only checkout of `url` at `revision` under `cache_root`.
///
/// Returns the checkout directory (`<cache_root>/<url-key>/checkouts/<oid>`).
///
/// When `update` is true, `revision` is treated as a fetchable ref: the mirror
/// is refreshed, the ref fetched, and resolved to the object ID that keys the
/// checkout. When `update` is false, `revision` must already be a full object ID
/// (a locked revision); a valid existing checkout is reused without touching the
/// network, otherwise the mirror is refreshed only enough to supply the commit.
///
/// # Errors
///
/// Fails if the origin URL does not match, the revision cannot be resolved or is
/// not a valid object ID, or any Git or filesystem operation fails.
pub fn materialize(url: &str, revision: &str, cache_root: &Path, update: bool) -> anyhow::Result<PathBuf> {
    let source_cache = cache_root.join(poly_cache::remote_source_key(url));
    std::fs::create_dir_all(&source_cache)
        .with_context(|| format!("creating source cache {}", source_cache.display()))?;
    let mirror = source_cache.join("mirror.git");
    if update {
        ensure_mirror(&mirror, url)?;
        run_git(&mirror, &["fetch", "--quiet", "--force", "origin", revision])?;
        let resolved = git_output(&mirror, &["rev-parse", "FETCH_HEAD^{commit}"])?;
        validate_locked_revision(&resolved)?;
        let checkout = source_cache.join("checkouts").join(&resolved);
        materialize_checkout(&mirror, &checkout, &resolved)?;
        return Ok(checkout);
    }
    validate_locked_revision(revision)?;
    let checkout = source_cache.join("checkouts").join(revision);
    if checkout_is_valid(&checkout, revision) {
        make_read_only(&checkout)?;
        return Ok(checkout);
    }
    ensure_mirror(&mirror, url)?;
    ensure_commit(&mirror, url, revision)?;
    materialize_checkout(&mirror, &checkout, revision)?;
    Ok(checkout)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn create_git_source() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        run_command(
            Command::new("git").arg("init").arg("--quiet").arg(repository.path()),
            "initialize test source",
        )
        .unwrap();
        std::fs::write(repository.path().join("catalog.txt"), "content\n").unwrap();
        run_git(repository.path(), &["add", "catalog.txt"]).unwrap();
        run_command(
            Command::new("git").arg("-C").arg(repository.path()).args([
                "-c",
                "user.name=Poly Test",
                "-c",
                "user.email=poly@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ]),
            "commit test source",
        )
        .unwrap();
        repository
    }

    #[test]
    fn materialize_resolves_ref_then_reuses_locked_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let url = producer.path().to_string_lossy().into_owned();

        // update=true resolves the ref and produces an OID-keyed read-only checkout.
        let checkout = materialize(&url, "HEAD", cache.path(), true).unwrap();
        assert!(checkout.is_dir());
        assert!(
            std::fs::metadata(checkout.join("catalog.txt"))
                .unwrap()
                .permissions()
                .readonly()
        );
        let oid = checkout.file_name().unwrap().to_string_lossy().into_owned();
        validate_locked_revision(&oid).unwrap();

        // update=false with the resolved OID reuses the identical checkout.
        let reused = materialize(&url, &oid, cache.path(), false).unwrap();
        assert_eq!(reused, checkout);
    }

    #[test]
    fn materialize_rejects_non_oid_locked_revision() {
        let cache = tempfile::tempdir().unwrap();
        let error = materialize("https://example.invalid/repo.git", "../../outside", cache.path(), false).unwrap_err();
        assert!(error.to_string().contains("full hexadecimal Git object ID"));
    }
}
