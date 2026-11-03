//! The cache pruning plan and its execution.
//!
//! A prune is a plan (which objects and crash leftovers to remove) followed by an execution
//! (remove them, skipping what the OS refuses). `--dry-run` prints the plan and a real prune
//! executes the very same plan, so the two cannot disagree.
//!
//! Last-used time comes from alef: every load goes through `ArtifactCache::install`, which touches
//! a `<sha256>.used` file next to the object once per process per component, best effort, and
//! `CachedObject::last_used` falls back to the install time when the file is absent. poly keeps no
//! stamp of its own.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use alef_component_runtime::{CachedObject, ComponentLock};

/// Staging directories and temp files younger than this may belong to a live install in another
/// process and are left alone. Matches alef's own sweep.
pub const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// What a candidate is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An installed object directory.
    Object,
    /// An orphaned staging directory, temp file or sidecar.
    Leftover,
}

/// One path a prune would remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The path to remove.
    pub path: PathBuf,
    /// Whether it is an object or a leftover.
    pub kind: Kind,
    /// Bytes it occupies.
    pub size_bytes: u64,
    /// The component of this lock the object belongs to, when it has one.
    pub component: Option<String>,
}

/// What executing a plan did.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Removed candidates.
    pub removed: Vec<Candidate>,
    /// Candidates the OS refused to delete because a loaded library holds them, with the error.
    pub skipped: Vec<(Candidate, String)>,
}

/// Whether `error` is a Windows sharing or lock violation (a loaded DLL cannot be deleted).
///
/// The raw codes 32 (`ERROR_SHARING_VIOLATION`) and 33 (`ERROR_LOCK_VIOLATION`) are matched on
/// every platform so the check is testable anywhere; on Unix a delete never produces them.
/// `PermissionDenied` and code 5 count only on Windows, where they are how a held file reports.
#[must_use]
pub fn is_sharing_violation(error: &io::Error) -> bool {
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    match error.raw_os_error() {
        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION) => true,
        Some(ERROR_ACCESS_DENIED) => cfg!(windows),
        _ => cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied,
    }
}

/// Every archive digest the lock names, across all targets.
#[must_use]
pub fn locked_digests(lock_json: Option<&str>) -> HashSet<String> {
    lock_json
        .and_then(|json| serde_json::from_str::<ComponentLock>(json).ok())
        .map(|lock| lock.artifacts.into_iter().map(|entry| entry.sha256).collect())
        .unwrap_or_default()
}

/// What to remove.
///
/// An object is kept when `all` is false and either `referenced` names its digest (or it belongs to
/// a component of this target's lock) or it was used within `keep_days`. A lock from an older poly
/// version is not `referenced`, so what only it names is removed once stale. Leftovers are
/// `.install-*` staging directories and `.tmp*` files older than [`STALE_AFTER`], and sidecars
/// whose object is gone.
///
/// # Errors
///
/// When the cache directory cannot be read.
pub fn plan(
    cache_root: &Path,
    objects: &[CachedObject],
    referenced: &HashSet<String>,
    all: bool,
    keep_days: u64,
    now: SystemTime,
) -> io::Result<Vec<Candidate>> {
    let window = Duration::from_secs(keep_days.saturating_mul(SECONDS_PER_DAY));
    let mut candidates = Vec::new();
    for object in objects {
        let used_recently = object
            .last_used
            .is_some_and(|used| !now.duration_since(used).is_ok_and(|age| age > window));
        let kept = !all && (referenced.contains(&object.sha256) || object.component.is_some() || used_recently);
        if !kept {
            candidates.push(Candidate {
                path: object.path.clone(),
                kind: Kind::Object,
                size_bytes: object.size_bytes,
                component: object.component.clone(),
            });
        }
    }
    candidates.extend(leftovers(&cache_root.join("sha256"), now)?);
    Ok(candidates)
}

fn leftovers(objects_dir: &Path, now: SystemTime) -> io::Result<Vec<Candidate>> {
    let reader = match fs::read_dir(objects_dir) {
        Ok(reader) => reader,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut found = Vec::new();
    for item in reader {
        let item = item?;
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = item.path();
        let is_dir = item.file_type()?.is_dir();
        let stale = if (name.starts_with(".install-") && is_dir) || (name.starts_with(".tmp") && !is_dir) {
            fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= STALE_AFTER)
        } else if let Some(digest) = sidecar_digest(&name) {
            !is_dir && !objects_dir.join(digest).is_dir() && sidecar_is_unclaimed(&path, &name)
        } else {
            false
        };
        if stale {
            let size_bytes = if is_dir {
                dir_size(&path)
            } else {
                item.metadata()?.len()
            };
            found.push(Candidate {
                path,
                kind: Kind::Leftover,
                size_bytes,
                component: None,
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Remove every candidate with `remover`, skipping those it refuses with a sharing violation.
///
/// # Errors
///
/// On any other removal error.
pub fn execute(candidates: Vec<Candidate>, remover: &dyn Fn(&Path) -> io::Result<()>) -> io::Result<Outcome> {
    let mut outcome = Outcome::default();
    for candidate in candidates {
        match remover(&candidate.path) {
            Ok(()) => {
                if candidate.kind == Kind::Object {
                    remove_sidecars(&candidate.path);
                }
                outcome.removed.push(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if is_sharing_violation(&error) => {
                let reason = format!("in use by a loaded library, left for a later prune: {error}");
                outcome.skipped.push((candidate, reason));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(outcome)
}

/// Remove a file or a directory tree.
///
/// # Errors
///
/// When the path cannot be removed.
pub fn remove_path(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn remove_sidecars(object: &Path) {
    let (Some(dir), Some(name)) = (object.parent(), object.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    for suffix in ["used", "verified"] {
        let _ = fs::remove_file(dir.join(format!("{name}.{suffix}")));
    }
    let lock = dir.join(format!("{name}.lock"));
    if sidecar_is_unclaimed(&lock, &format!("{name}.lock")) {
        let _ = fs::remove_file(lock);
    }
}

fn is_digest_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sidecar_digest(name: &str) -> Option<&str> {
    let (digest, suffix) = name.split_once('.')?;
    (is_digest_name(digest) && matches!(suffix, "lock" | "used" | "verified")).then_some(digest)
}

/// A `.lock` file is only stale when no process holds it; other sidecars always are.
fn sidecar_is_unclaimed(path: &Path, name: &str) -> bool {
    if !name.ends_with(".lock") {
        return true;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .is_ok_and(|file| file.try_lock().is_ok())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(reader) = fs::read_dir(path) else {
        return 0;
    };
    reader
        .flatten()
        .map(|item| match item.file_type() {
            Ok(kind) if kind.is_dir() => dir_size(&item.path()),
            Ok(_) => item.metadata().map_or(0, |meta| meta.len()),
            Err(_) => 0,
        })
        .sum()
}
