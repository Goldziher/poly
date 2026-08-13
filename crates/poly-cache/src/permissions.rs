//! Restrictive permissions for the directories poly creates under the cache home.
//!
//! # Why
//!
//! The per-repo cache slot holds more than lint verdicts: the hook staged
//! snapshot (`<repo-cache>/staged`) is a full mirror of the repository's staged
//! source. Created with nothing but the process umask — commonly `022` — that
//! tree is world-readable, so on a shared multi-user host any other local
//! account can read a copy of the source being committed.
//!
//! # What poly does
//!
//! On Unix every directory poly creates under the cache home is created with
//! mode `0700` (`rwx------`) instead of the umask default. The mode passed to
//! `mkdir(2)` is an upper bound — the umask can only take bits away — so a
//! stricter umask still wins and no user ends up with a *looser* directory than
//! they asked for.
//!
//! Because Unix permission checks are per path component, an owner-only
//! directory also seals everything beneath it: once `<cache-home>/<repo-key>` is
//! `0700`, no other user can traverse into `staged/` or `results/` regardless of
//! the modes on those children.
//!
//! On Windows the concept does not map: the cache lives under the per-user
//! `%LOCALAPPDATA%`, which is already ACL-restricted to the owning account and
//! inherited by new subdirectories, so directory creation is left to the
//! platform default.
//!
//! # Existing directories: whoever chose the location decides
//!
//! Creating *new* directories owner-only does nothing for the far larger
//! population of cache slots an older poly already created under the umask
//! default, typically `0755`. What poly may do about those turns on
//! [`DirOrigin`] — on who chose the location. That is knowable only where the
//! path is resolved, and is unrecoverable afterwards: an overridden cache home
//! and the platform default are the same kind of string carrying the same mode
//! bits.
//!
//! - [`DirOrigin::PolyOwned`] — the platform per-user cache directory
//!   (`~/.cache/poly/…`, `~/Library/Caches/poly/…`) with no override in play.
//!   poly picked the path, poly created it, and nobody was ever invited to share
//!   it. A loose mode there is the fingerprint of the umask an older poly ran
//!   under, not a decision, so poly tightens it to `0700` silently. Warning
//!   instead would repeat on every run of every upgraded install, and a warning
//!   that never goes away only teaches people to ignore warnings — worse than
//!   the low-severity exposure it reports.
//! - [`DirOrigin::UserConfigured`] — a location named by `POLY_CACHE_HOME`, by
//!   `[cache] dir`, or handed to `ResultCache::open` directly. Here the original
//!   reasoning still holds in full: the mode may encode a deliberate choice — a
//!   shared CI cache, a team directory — and the mode alone carries no way to
//!   tell "created under a loose umask" from "shared on purpose". Silently
//!   tightening it would break the other consumers of a shared cache mid-run, a
//!   failure far harder to diagnose than the one it prevents. poly leaves it
//!   exactly as it is and warns once with the command that fixes it, leaving the
//!   decision to the user.
//!
//! Tightening is best-effort and never fatal. poly chmods only a directory it
//! actually owns (`st_uid` equal to the effective uid), because rewriting the
//! mode of another account's directory is precisely the decision that is not
//! poly's to make; and a chmod that fails anyway — read-only mount, immutable
//! flag — degrades to the same warning the configured case gets, so the user
//! still hears about it.

use std::path::Path;

/// Who chose the directory poly is about to use, and therefore whether poly may
/// rewrite the mode of one that already exists.
///
/// Captured at resolution time and carried alongside the path — see the module
/// docs for why it cannot be recovered from the path itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirOrigin {
    /// Resolved from the platform per-user cache directory with no
    /// `POLY_CACHE_HOME` and no `[cache] dir` override: poly's own directory,
    /// which poly may tighten.
    PolyOwned,
    /// A location the user named explicitly. Its mode is the user's to choose,
    /// so poly only reports a loose one.
    UserConfigured,
}

/// Directory mode for everything poly creates under the cache home: owner-only
/// read, write, and traverse (`rwx------`).
#[cfg(unix)]
pub const PRIVATE_DIR_MODE: u32 = 0o700;

/// Permission bits that make a directory reachable by group or other.
#[cfg(unix)]
const GROUP_OR_OTHER_BITS: u32 = 0o077;

/// Create `path` and any missing parent, owner-only on Unix.
///
/// Equivalent to [`std::fs::create_dir_all`] except that every directory it
/// creates gets [`PRIVATE_DIR_MODE`] rather than the umask default. Directories
/// that already exist are left exactly as they are; deciding what to do about
/// those is [`ensure_private_dir`]'s job.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when a directory cannot be created.
#[cfg(unix)]
pub fn create_dir_all_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
}

/// Create `path` and any missing parent.
///
/// On non-Unix platforms this is plain [`std::fs::create_dir_all`]: the cache
/// lives under the per-user application-data directory, whose inherited ACL is
/// the platform's equivalent of the Unix mode bits.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when a directory cannot be created.
#[cfg(not(unix))]
pub fn create_dir_all_private(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Whether `mode` leaves a directory reachable by group or other.
#[cfg(unix)]
fn is_reachable_by_others(mode: u32) -> bool {
    mode & GROUP_OR_OTHER_BITS != 0
}

/// Tighten an existing poly-owned cache directory to [`PRIVATE_DIR_MODE`],
/// degrading to [`warn_if_reachable_by_others`] when poly cannot.
///
/// Every step is best-effort: an un-hardened cache directory is a low-severity
/// exposure, and failing a lint run over it would be the worse trade. Ownership
/// is checked first — a directory belonging to another account is not poly's to
/// rewrite, and on a shared host that is exactly the case the warning exists
/// for.
#[cfg(unix)]
fn tighten_or_warn(path: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if !is_reachable_by_others(metadata.permissions().mode() & 0o777) {
        return;
    }
    let tightened = metadata.uid() == rustix::process::geteuid().as_raw()
        && std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_DIR_MODE)).is_ok();
    if !tightened {
        warn_if_reachable_by_others(path);
    }
}

/// No-op on non-Unix platforms, where directory access is governed by inherited
/// ACLs rather than mode bits.
#[cfg(not(unix))]
fn tighten_or_warn(_path: &Path) {}

/// Warn — at most once per directory per process — when a cache directory poly
/// must not modify can be reached by other local users.
///
/// The warning names the directory and the fix rather than applying it, because
/// at a location someone configured the loose mode may be intentional (see the
/// module docs). De-duplicating per directory rather than per process keeps one
/// run from repeating itself while still reporting a *second*, different loose
/// directory; a run resolves a handful of cache directories, so the set stays
/// tiny.
#[cfg(unix)]
pub fn warn_if_reachable_by_others(path: &Path) {
    use std::collections::HashSet;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::{LazyLock, Mutex};

    static WARNED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if !is_reachable_by_others(mode) {
        return;
    }
    let first_time = match WARNED.lock() {
        Ok(mut seen) => seen.insert(path.to_path_buf()),
        // A poisoned set only means some other thread panicked mid-insert;
        // warning twice beats swallowing the warning entirely.
        Err(poisoned) => poisoned.into_inner().insert(path.to_path_buf()),
    };
    if !first_time {
        return;
    }
    tracing::warn!(
        dir = %path.display(),
        mode = format_args!("{mode:04o}"),
        fix = %format_args!("chmod 700 {}", path.display()),
        "cache directory is readable by other local users; poly never changes the mode of a cache \
         location you configured yourself — ignore this if the location is shared deliberately",
    );
}

/// No-op on non-Unix platforms, where directory access is governed by inherited
/// ACLs rather than mode bits.
#[cfg(not(unix))]
pub fn warn_if_reachable_by_others(_path: &Path) {}

/// [`create_dir_all_private`] followed by the policy for whatever was already
/// there — the combination every cache-root creation wants.
///
/// A [`DirOrigin::PolyOwned`] directory is tightened to [`PRIVATE_DIR_MODE`]; a
/// [`DirOrigin::UserConfigured`] one is left alone and merely reported. See the
/// module docs for why the two differ.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be created.
pub fn ensure_private_dir(path: &Path, origin: DirOrigin) -> std::io::Result<()> {
    create_dir_all_private(path)?;
    match origin {
        DirOrigin::PolyOwned => tighten_or_warn(path),
        DirOrigin::UserConfigured => warn_if_reachable_by_others(path),
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::tests::warnings_during;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    /// A `0755` directory in a fresh tempdir, standing in for a cache slot an
    /// older poly created under a `022` umask.
    fn loose_dir(tmp: &tempfile::TempDir, name: &str) -> std::path::PathBuf {
        let dir = tmp.path().join(name);
        std::fs::create_dir(&dir).expect("create");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        dir
    }

    #[test]
    fn create_dir_all_private_creates_owner_only_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let nested = tmp.path().join("outer").join("inner");

        create_dir_all_private(&nested).expect("create");

        assert_eq!(mode_of(&nested), PRIVATE_DIR_MODE, "leaf must be owner-only");
        assert_eq!(
            mode_of(&tmp.path().join("outer")),
            PRIVATE_DIR_MODE,
            "intermediate directories must be owner-only too"
        );
    }

    #[test]
    fn create_dir_all_private_is_idempotent_and_leaves_existing_modes_alone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = loose_dir(&tmp, "shared");

        create_dir_all_private(&dir).expect("create again");

        assert_eq!(
            mode_of(&dir),
            0o755,
            "plain creation must never rewrite an existing directory's mode"
        );
    }

    #[test]
    fn is_reachable_by_others_flags_any_group_or_other_bit() {
        assert!(!is_reachable_by_others(0o700), "owner-only is not reachable");
        assert!(!is_reachable_by_others(0o600), "no traverse bits at all");
        assert!(is_reachable_by_others(0o750), "group execute is reachable");
        assert!(is_reachable_by_others(0o755), "the umask 022 default is reachable");
        assert!(is_reachable_by_others(0o701), "other execute alone is reachable");
    }

    #[test]
    fn a_poly_owned_loose_directory_is_tightened_and_stays_silent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = loose_dir(&tmp, "poly-owned");

        let warnings = warnings_during(|| {
            ensure_private_dir(&dir, DirOrigin::PolyOwned).expect("ensure");
        });

        assert_eq!(mode_of(&dir), 0o700, "a directory poly owns must be tightened in place");
        assert!(
            warnings.is_empty(),
            "nothing is left for the user to decide, so nothing may be warned about: {warnings:?}"
        );
    }

    #[test]
    fn a_user_configured_loose_directory_keeps_its_mode_and_warns_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = loose_dir(&tmp, "user-configured");

        let warnings = warnings_during(|| {
            ensure_private_dir(&dir, DirOrigin::UserConfigured).expect("ensure");
            ensure_private_dir(&dir, DirOrigin::UserConfigured).expect("ensure again");
        });

        assert_eq!(
            mode_of(&dir),
            0o755,
            "a location the user chose may be shared on purpose and must not be tightened"
        );
        assert_eq!(
            warnings.len(),
            1,
            "the warning is de-duplicated per directory: {warnings:?}"
        );
        let warning = &warnings[0];
        assert!(
            warning.contains(&dir.display().to_string()),
            "must name the directory: {warning}"
        );
        assert!(warning.contains("0755"), "must report the mode it found: {warning}");
        assert!(warning.contains("chmod 700"), "must offer the fix: {warning}");
    }

    #[test]
    fn an_already_private_directory_is_neither_touched_nor_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("private");
        create_dir_all_private(&dir).expect("create");

        let warnings = warnings_during(|| {
            ensure_private_dir(&dir, DirOrigin::PolyOwned).expect("poly-owned");
            ensure_private_dir(&dir, DirOrigin::UserConfigured).expect("user-configured");
        });

        assert_eq!(mode_of(&dir), 0o700, "an owner-only directory stays owner-only");
        assert!(
            warnings.is_empty(),
            "nothing is wrong, so nothing is reported: {warnings:?}"
        );
    }
}
