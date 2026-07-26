//! Detached, read-only working-tree checkouts materialized from a bare mirror.
//!
//! Checkouts are keyed by the resolved object ID under `<source>/checkouts/<oid>`
//! and made read-only after materialization; a checkout whose HEAD drifts or
//! whose tree is tampered with is treated as invalid and rebuilt.

use std::path::Path;
use std::process::Command;

use anyhow::Context;

use super::git::{git_output, run_command, run_git};

/// Materialize a detached checkout of `revision` from `mirror` at `checkout`.
///
/// If a valid checkout already exists it is left in place (and re-made
/// read-only); an invalid one is replaced. The new tree is cloned into a
/// sibling tempdir, checked out detached, atomically renamed into place, and
/// made read-only so nothing downstream can mutate the shared cache entry.
pub fn materialize_checkout(mirror: &Path, checkout: &Path, revision: &str) -> anyhow::Result<()> {
    if checkout.is_dir() {
        if checkout_is_valid(checkout, revision) {
            return make_read_only(checkout);
        }
        make_writable(checkout)?;
        std::fs::remove_dir_all(checkout)
            .with_context(|| format!("removing invalid source checkout {}", checkout.display()))?;
    }
    let parent = checkout.parent().context("source checkout has no parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating source checkout directory {}", parent.display()))?;
    let temporary = tempfile::Builder::new()
        .prefix("checkout-")
        .tempdir_in(parent)
        .with_context(|| format!("creating temporary source checkout in {}", parent.display()))?;
    let temporary_path = temporary.path().join("source");
    run_command(
        Command::new("git")
            .args(["clone", "--quiet", "--no-checkout", "--no-hardlinks"])
            .arg(mirror)
            .arg(&temporary_path),
        "clone source checkout",
    )?;
    run_git(&temporary_path, &["checkout", "--quiet", "--detach", revision])?;
    std::fs::rename(&temporary_path, checkout)
        .with_context(|| format!("installing source checkout {}", checkout.display()))?;
    make_read_only(checkout)
}

/// Whether `checkout` is a clean working tree whose HEAD is exactly `revision`.
pub fn checkout_is_valid(checkout: &Path, revision: &str) -> bool {
    if !checkout.is_dir() {
        return false;
    }
    let head = git_output(checkout, &["rev-parse", "HEAD^{commit}"]);
    if !matches!(head.as_deref(), Ok(value) if value == revision) {
        return false;
    }
    matches!(
        git_output(checkout, &["status", "--porcelain=v1", "--untracked-files=all"]),
        Ok(status) if status.is_empty()
    )
}

/// Strip the write bit from every file in `root` (recursively).
pub fn make_read_only(root: &Path) -> anyhow::Result<()> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.with_context(|| format!("walking source checkout {}", root.display()))?;
        if entry.file_type().is_symlink() {
            continue;
        }
        let mut permissions = entry.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() & !0o222);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(true);
        std::fs::set_permissions(entry.path(), permissions)
            .with_context(|| format!("making source checkout read-only: {}", entry.path().display()))?;
    }
    Ok(())
}

/// Restore owner write permission across `root` so it can be removed or rebuilt.
#[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))]
pub fn make_writable(root: &Path) -> anyhow::Result<()> {
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.with_context(|| format!("walking source checkout {}", root.display()))?;
        if entry.file_type().is_symlink() {
            continue;
        }
        let mut permissions = entry.metadata()?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o700);
        }
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        std::fs::set_permissions(entry.path(), permissions)
            .with_context(|| format!("making source checkout writable: {}", entry.path().display()))?;
    }
    Ok(())
}
