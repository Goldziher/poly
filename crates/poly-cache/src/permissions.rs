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
//! # What poly deliberately does not do
//!
//! An *existing* directory is never chmod-ed. Its mode may encode a deliberate
//! choice — a shared CI cache under `POLY_CACHE_HOME`, a team directory pinned
//! with `[cache] dir` — and the mode alone carries no way to tell "created under
//! a loose umask" from "shared on purpose". Silently tightening it would break
//! the other consumers of a shared cache mid-run, a failure far harder to
//! diagnose than the one it prevents. Instead poly warns once per process with
//! the command that fixes it, and leaves the decision to the user.

use std::path::Path;

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
/// that already exist are left exactly as they are — see the module docs.
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

/// Warn — at most once per process — when an existing cache directory can be
/// reached by other local users.
///
/// Only pre-existing directories can trip this: anything poly creates itself is
/// already owner-only. The warning names the directory and the fix rather than
/// applying it, because the loose mode may be intentional (see the module docs).
#[cfg(unix)]
pub fn warn_if_reachable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Once;

    static WARNED: Once = Once::new();

    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let mode = metadata.permissions().mode() & 0o777;
    if !is_reachable_by_others(mode) {
        return;
    }
    WARNED.call_once(|| {
        tracing::warn!(
            dir = %path.display(),
            mode = format_args!("{mode:04o}"),
            fix = %format_args!("chmod 700 {}", path.display()),
            "cache directory is readable by other local users; poly creates cache directories \
             owner-only but never changes one that already exists — ignore this if the location \
             is shared deliberately",
        );
    });
}

/// No-op on non-Unix platforms, where directory access is governed by inherited
/// ACLs rather than mode bits.
#[cfg(not(unix))]
pub fn warn_if_reachable_by_others(_path: &Path) {}

/// [`create_dir_all_private`] followed by [`warn_if_reachable_by_others`], the
/// combination every cache-root creation wants: create it owner-only, and flag
/// it when it was already there and is open to other users.
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when the directory cannot be created.
pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    create_dir_all_private(path)?;
    warn_if_reachable_by_others(path);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
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
        let dir = tmp.path().join("shared");
        std::fs::create_dir(&dir).expect("create");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        create_dir_all_private(&dir).expect("create again");

        assert_eq!(
            mode_of(&dir),
            0o755,
            "an existing directory's mode is the user's choice and must not be rewritten"
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
}
