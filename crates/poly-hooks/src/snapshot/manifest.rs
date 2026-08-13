//! The snapshot's incremental ledger: `path → (index OID, size, mtime)`.
//!
//! Split from the parent module because this is the one piece of the snapshot
//! that is a *record format* rather than a filesystem action — it is read,
//! parsed, compared, and written back, and every decision the refresh makes
//! about **what to skip and what to delete** is derived from it. Keeping the
//! record type, its wire format, and the two predicates that interpret it
//! ([`is_up_to_date`], [`prune_stale`]) in one file means [`Record`] and
//! [`Stat`] need no visibility outside it, so nothing else can compare them
//! by hand and get the staleness rule subtly wrong.
//!
//! The semantics — why the stat is folded in alongside the OID, and why an
//! unreadable manifest re-materializes everything — are documented on the
//! parent module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::Error;
use crate::git;

/// Manifest recording the tracked paths materialized last run, so prune removes
/// only files that fell out of the tree — never tool-generated caches.
pub(super) const MANIFEST_FILE: &str = ".poly-manifest";

/// Manifest placeholder for a stat field that could not be read when the record
/// was written. It never compares equal to a real stat, so the path is
/// re-materialized on the next refresh while still taking part in the prune.
const UNKNOWN_STAT: &str = "-";

/// What the manifest records for one materialized path: the index OID it was
/// written from plus the `(size, mtime)` observed immediately afterwards.
///
/// The stat is `None` when it could not be read at write time, which never
/// matches a later observation and so forces a re-materialization.
/// Visible to the parent module only so the refresh can hold the map this
/// module hands it; every field stays private, so the staleness rule can only
/// be applied through [`is_up_to_date`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Record {
    oid: String,
    stat: Option<Stat>,
}

/// Size and modification time of a materialized snapshot file — the fingerprint
/// that detects a write by anything other than `git checkout-index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime_nanos: u128,
}

impl Stat {
    /// Read the fingerprint of `path`, or `None` when it is absent or its mtime
    /// is unrepresentable. `symlink_metadata` is used so a materialized symlink
    /// is fingerprinted as itself rather than as its target.
    fn read(path: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(path).ok()?;
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Self {
            size: meta.len(),
            mtime_nanos: mtime.as_nanos(),
        })
    }
}

/// Whether the snapshot copy of `entry` can be left untouched: the index OID is
/// unchanged **and** the file on disk is still byte-for-byte the one we wrote
/// for that OID, as far as its `(size, mtime)` can attest.
pub(super) fn is_up_to_date(dir: &Path, entry: &git::StagedEntry, previous: &HashMap<PathBuf, Record>) -> bool {
    let Some(record) = previous.get(&entry.path) else {
        return false;
    };
    record.oid == entry.oid && record.stat.is_some() && record.stat == Stat::read(&dir.join(&entry.path))
}

/// Remove snapshot files from the previous manifest that are no longer staged.
/// Restricting deletion to the manifest means tool caches written into the
/// snapshot (`target/`, `.mypy_cache`, …) are never touched.
pub(super) fn prune_stale(dir: &Path, staged: &[git::StagedEntry], previous: &HashMap<PathBuf, Record>) {
    let current: std::collections::HashSet<&PathBuf> = staged.iter().map(|entry| &entry.path).collect();
    for path in previous.keys() {
        if !current.contains(path) {
            let _ = std::fs::remove_file(dir.join(path));
        }
    }
}

/// Read the previous manifest into a path → [`Record`] map (NUL-separated
/// `<oid> <size> <mtime> <path>` records). An absent or unreadable manifest
/// yields an empty map, so everything is re-materialized once — the safe
/// direction.
pub(super) fn read_manifest(dir: &Path) -> HashMap<PathBuf, Record> {
    std::fs::read(dir.join(MANIFEST_FILE))
        .map(|bytes| {
            bytes
                .split(|&byte| byte == 0)
                .filter(|slice| !slice.is_empty())
                .filter_map(parse_manifest_record)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse one `<oid> <size> <mtime> <path>` manifest record. The leading fields
/// are space-free, so the path is whatever follows the third space and is taken
/// verbatim (it may itself contain spaces).
///
/// A record written by an older poly carries no stat fields (`<oid> <path>`); it
/// is accepted with `stat: None` so an upgrade keeps the prune ledger and merely
/// re-materializes every path once.
fn parse_manifest_record(record: &[u8]) -> Option<(PathBuf, Record)> {
    let (oid, rest) = split_field(record)?;
    let oid = std::str::from_utf8(oid).ok()?.to_string();
    let (stat, path_bytes) = parse_stat_fields(rest).unwrap_or((None, rest));
    let path = git::path_from_git_bytes(path_bytes).ok()?;
    Some((path, Record { oid, stat }))
}

/// Split the leading space-delimited field off `record`, returning it and the
/// remainder. `None` when the record has no space (so no path can follow).
fn split_field(record: &[u8]) -> Option<(&[u8], &[u8])> {
    let space = record.iter().position(|&byte| byte == b' ')?;
    Some((&record[..space], &record[space + 1..]))
}

/// Parse the `<size> <mtime>` fields, returning them with the remaining path
/// bytes. `None` when `rest` does not start with two such fields — i.e. it is a
/// legacy stat-less record whose path begins right here.
fn parse_stat_fields(rest: &[u8]) -> Option<(Option<Stat>, &[u8])> {
    let (size, rest) = split_field(rest)?;
    let (mtime, rest) = split_field(rest)?;
    let size = std::str::from_utf8(size).ok()?;
    let mtime = std::str::from_utf8(mtime).ok()?;
    if (size, mtime) == (UNKNOWN_STAT, UNKNOWN_STAT) {
        return Some((None, rest));
    }
    let stat = Stat {
        size: size.parse().ok()?,
        mtime_nanos: mtime.parse().ok()?,
    };
    Some((Some(stat), rest))
}

/// Write the manifest for the currently-staged paths, fingerprinting each
/// materialized file as it goes (NUL-separated `<oid> <size> <mtime> <path>`
/// records). A file whose stat cannot be read is recorded with [`UNKNOWN_STAT`]
/// placeholders: it stays in the prune ledger but is re-materialized next run.
pub(super) fn write_manifest(dir: &Path, staged: &[git::StagedEntry]) -> Result<(), Error> {
    let mut bytes = Vec::new();
    for entry in staged {
        let stat = Stat::read(&dir.join(&entry.path)).map_or_else(
            || format!("{UNKNOWN_STAT} {UNKNOWN_STAT}"),
            |stat| format!("{} {}", stat.size, stat.mtime_nanos),
        );
        bytes.extend_from_slice(entry.oid.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(stat.as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(path_to_git_bytes(&entry.path).as_ref());
        bytes.push(0);
    }
    std::fs::write(dir.join(MANIFEST_FILE), bytes)?;
    Ok(())
}

/// Encode a repo-relative path for the manifest, byte-faithfully on unix so a
/// non-UTF-8 path round-trips through [`git::path_from_git_bytes`] instead of
/// being lossily mangled (which would re-materialize it on every run).
#[cfg(unix)]
fn path_to_git_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt as _;

    std::borrow::Cow::Borrowed(path.as_os_str().as_bytes())
}

#[cfg(not(unix))]
fn path_to_git_bytes(path: &Path) -> std::borrow::Cow<'_, [u8]> {
    match path.to_string_lossy() {
        std::borrow::Cow::Borrowed(text) => std::borrow::Cow::Borrowed(text.as_bytes()),
        std::borrow::Cow::Owned(text) => std::borrow::Cow::Owned(text.into_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_records_round_trip_including_paths_with_spaces() {
        let parsed = parse_manifest_record(b"abc123 42 7 dir/a file.rs").expect("parse");
        assert_eq!(parsed.0, PathBuf::from("dir/a file.rs"));
        assert_eq!(
            parsed.1,
            Record {
                oid: "abc123".to_string(),
                stat: Some(Stat {
                    size: 42,
                    mtime_nanos: 7
                }),
            }
        );

        let unknown = parse_manifest_record(b"abc123 - - a.rs").expect("parse unknown stat");
        assert_eq!(unknown.1.stat, None, "an unknown stat must never match a real one");

        // A manifest written by an older poly has no stat fields.
        let legacy = parse_manifest_record(b"abc123 a.rs").expect("parse legacy");
        assert_eq!(legacy.0, PathBuf::from("a.rs"));
        assert_eq!(legacy.1.stat, None);
    }
}
