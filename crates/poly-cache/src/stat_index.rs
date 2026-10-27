//! Stat-validated file verdict index: the warm-run fast path.
//!
//! The content-addressed result cache answers "what did engine E say about
//! *these bytes*", which still costs reading and hashing every file before the
//! first lookup. For the overwhelmingly common outcome — the file is clean and
//! has not changed — that work is the whole warm run. This index answers the
//! question one level up, without opening the file: "was this path, with this
//! `stat`, already judged clean under this exact plan?".
//!
//! # Records
//!
//! A [`Record`] holds, per path: the `stat` it was observed under, a digest of
//! the bytes, an overall [`Verdict`], and one [`Segment`] per engine — the key
//! that engine ran under and its raw output. That makes it both the fast path
//! and the per-engine result cache:
//!
//! - **`stat` hit** — the stat, the run's scope key and *every* engine key equal
//!   the recorded ones: the verdict is replayed without opening the file.
//! - **content hit** — the file was read and its digest equals the recorded one:
//!   each engine whose key still matches reuses its segment, so editing one
//!   engine's configuration re-runs only that engine, and a touched-but-identical
//!   file re-runs none.
//!
//! The engine key folds in the cache format, the build identity, the engine's id,
//! version and arguments, and its declared cross-file inputs, so a config edit,
//! an engine upgrade or a changed sibling `__init__.py` is a miss, never a stale
//! hit.
//!
//! # Racy timestamps
//!
//! `stat` equality proves "unchanged" only when a modification would have moved
//! the stat. It would not if the file was rewritten within the filesystem's
//! timestamp granularity of the earlier write, keeping size and mtime. Git's
//! answer is to distrust entries whose mtime is not strictly older than the
//! index; [`StatIndex::put`] applies the same rule at record time: a file whose
//! mtime or ctime is within [`RACY_WINDOW_NS`] of *now* is recorded with
//! [`FileStat::UNTRUSTED`], so only a content check can vouch for it on the next
//! run, by which time the timestamp has aged out of the window. ctime and inode
//! are part of the stat as a second line of defence — a same-size, same-mtime
//! rewrite (`touch -r`, `utimes`) still moves ctime, which no userspace call can
//! set.
//!
//! # Storage
//!
//! One file per namespace, `<root>/index/<namespace>.idx`: a 4-byte magic and
//! length-prefixed records sorted by path key. A clean file costs ~100 bytes plus
//! 20 per engine, rather than one filesystem entry per engine. Reads load the
//! whole file once; writes merge this run's records into the loaded set (or, if
//! another process replaced the file meanwhile, into what is on disk now) and
//! replace the file atomically, so concurrent runs lose at most each other's
//! newest records.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 4] = b"PSI2";
const HEADER_LEN: usize = MAGIC.len();
const ID_LEN: usize = 16;

/// How recently a file must have been modified, relative to the moment it is
/// recorded, for its record to be withheld. Two seconds covers the coarsest
/// timestamp granularity poly meets (FAT) and any clock skew between a network
/// filesystem and this host.
pub const RACY_WINDOW_NS: i64 = 2_000_000_000;

type Id = [u8; ID_LEN];

fn truncated(hash: &blake3::Hash) -> Id {
    let mut id = [0u8; ID_LEN];
    id.copy_from_slice(&hash.as_bytes()[..ID_LEN]);
    id
}

/// What `stat` reports about a file, compared whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    ino: u64,
}

impl FileStat {
    /// Stat `path`, following symlinks as a read would.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        std::fs::metadata(path).map(|metadata| Self::from_metadata(&metadata))
    }

    /// Build from metadata already in hand.
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                size: metadata.len(),
                mtime_ns: metadata.mtime().saturating_mul(1_000_000_000) + metadata.mtime_nsec(),
                ctime_ns: metadata.ctime().saturating_mul(1_000_000_000) + metadata.ctime_nsec(),
                ino: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                size: metadata.len(),
                mtime_ns: metadata.modified().map_or(0, system_time_ns),
                ctime_ns: 0,
                ino: 0,
            }
        }
    }

    /// A stat no real file has: a record carrying it is never vouched for by
    /// `stat` alone, only by its content digest.
    pub const UNTRUSTED: FileStat = FileStat {
        size: u64::MAX,
        mtime_ns: i64::MIN,
        ctime_ns: i64::MIN,
        ino: u64::MAX,
    };

    #[cfg(test)]
    pub(crate) fn synthetic(size: u64, mtime_ns: i64, ctime_ns: i64, ino: u64) -> Self {
        Self {
            size,
            mtime_ns,
            ctime_ns,
            ino,
        }
    }

    /// Whether a change made at `now_ns` could leave this stat unchanged.
    fn is_racy(&self, now_ns: i64) -> bool {
        let newest = self.mtime_ns.max(self.ctime_ns);
        newest > now_ns.saturating_sub(RACY_WINDOW_NS)
    }
}

#[cfg(not(unix))]
fn system_time_ns(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX))
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX))
}

/// A path reduced to a fixed-width key, made absolute so the same file reached
/// as `./a.py` and `a.py` is one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathKey(Id);

impl PathKey {
    /// Key `path`.
    pub fn of(path: &Path) -> Self {
        let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Self(truncated(&blake3::hash(absolute.as_os_str().as_encoded_bytes())))
    }
}

/// Digest of a file's bytes, for the touched-but-identical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentDigest(Id);

impl ContentDigest {
    /// Digest `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(truncated(&blake3::hash(bytes)))
    }
}

/// Everything besides a file's bytes that decided its verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanKey(Id);

impl PlanKey {
    /// Start a key for `namespace`, pre-seeded with the cache format and the
    /// running build's identity so no record outlives either.
    pub fn builder(namespace: crate::Namespace) -> PlanKeyBuilder {
        let mut hasher = blake3::Hasher::new();
        for part in [crate::CACHE_FORMAT_VERSION, crate::build_identity(), namespace.as_dir()] {
            feed(&mut hasher, part.as_bytes());
        }
        PlanKeyBuilder { hasher }
    }
}

impl PlanKey {
    /// This key extended with more inputs, for the ones only a single file has.
    pub fn extended<'a>(&self, parts: impl IntoIterator<Item = &'a str>) -> PlanKey {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.0);
        for part in parts {
            feed(&mut hasher, part.as_bytes());
        }
        PlanKey(truncated(&hasher.finalize()))
    }
}

/// Accumulates the parts of a [`PlanKey`]; every part is length-prefixed so two
/// different splits of the same bytes cannot collide.
#[derive(Debug, Clone)]
pub struct PlanKeyBuilder {
    hasher: blake3::Hasher,
}

fn feed(hasher: &mut blake3::Hasher, part: &[u8]) {
    hasher.update(&(part.len() as u64).to_le_bytes());
    hasher.update(part);
}

impl PlanKeyBuilder {
    /// Fold in one more input.
    pub fn part(&mut self, part: impl AsRef<[u8]>) -> &mut Self {
        feed(&mut self.hasher, part.as_ref());
        self
    }

    /// Finish the key.
    pub fn finish(&self) -> PlanKey {
        PlanKey(truncated(&self.hasher.finalize()))
    }
}

/// What a recorded file was judged to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every engine ran and had nothing to report (lint) or nothing to change (fmt).
    Clean,
    /// A binary file, skipped before any engine was consulted.
    Binary,
    /// At least one engine reported findings; they are in the segments.
    Diagnostics,
}

impl Verdict {
    fn to_byte(self) -> u8 {
        match self {
            Verdict::Clean => 1,
            Verdict::Binary => 2,
            Verdict::Diagnostics => 3,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Verdict::Clean),
            2 => Some(Verdict::Binary),
            3 => Some(Verdict::Diagnostics),
            _ => None,
        }
    }
}

/// One engine's contribution to a file's record: the key it ran under and what
/// it reported. An empty payload means it found nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Identifies the engine, its version, its arguments and its per-file
    /// inputs; a segment is only reused under an equal key.
    pub key: PlanKey,
    /// The engine's raw output, in whatever encoding the caller chose.
    pub payload: Vec<u8>,
}

/// Everything remembered about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// `stat` taken before the bytes were read, or [`FileStat::UNTRUSTED`].
    pub stat: FileStat,
    /// The run-level switches the verdict depends on, besides the engines.
    pub scope: PlanKey,
    /// Digest of the bytes the segments were computed from.
    pub digest: ContentDigest,
    /// The overall verdict.
    pub verdict: Verdict,
    /// Whether the verdict can be replayed without reading the file: no inline
    /// suppression directives, whose handling needs the content.
    pub replayable: bool,
    /// One per engine, in plan order.
    pub segments: Vec<Segment>,
}

impl Record {
    /// Whether the recorded `stat` vouches for a file that now looks like `stat`.
    pub fn trusts(&self, stat: &FileStat) -> bool {
        self.stat == *stat
    }

    /// Whether the segments were made under exactly these engine keys.
    pub fn has_keys(&self, keys: &[PlanKey]) -> bool {
        self.segments.len() == keys.len() && self.segments.iter().zip(keys).all(|(s, k)| s.key == *k)
    }
}

/// The per-namespace record store. See the [module docs](self).
#[derive(Debug)]
pub struct StatIndex {
    file: Option<PathBuf>,
    loaded: HashMap<PathKey, Record>,
    seen: Option<FileFingerprint>,
    pending: Mutex<HashMap<PathKey, Record>>,
}

/// Enough of a file's metadata to tell whether another process replaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    mtime_ns: i64,
}

fn fingerprint(file: &Path) -> Option<FileFingerprint> {
    let metadata = std::fs::metadata(file).ok()?;
    Some(FileFingerprint {
        len: metadata.len(),
        mtime_ns: FileStat::from_metadata(&metadata).mtime_ns,
    })
}

impl StatIndex {
    /// An index that never hits and never records, for a disabled cache.
    pub fn disabled() -> Self {
        Self {
            file: None,
            loaded: HashMap::new(),
            seen: None,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Load the index stored at `file`; a missing, truncated or foreign file is
    /// an empty index rather than an error.
    pub fn load(file: PathBuf) -> Self {
        let seen = fingerprint(&file);
        let loaded = read_records(&file);
        Self {
            file: Some(file),
            loaded,
            seen,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The record loaded for `path`, if any.
    pub fn get(&self, path: &PathKey) -> Option<&Record> {
        self.loaded.get(path)
    }

    /// Remember `record` for `path`, observed with a `stat` taken **before** the
    /// bytes behind its digest were read. A file modified within the racy window
    /// is still remembered — its digest and segments stay reusable by content —
    /// but with [`FileStat::UNTRUSTED`], so `stat` alone never vouches for it.
    pub fn put(&self, path: PathKey, record: Record) {
        self.put_at(now_ns(), path, record);
    }

    pub(crate) fn put_at(&self, now_ns: i64, path: PathKey, mut record: Record) {
        if self.file.is_none() {
            return;
        }
        if record.stat.is_racy(now_ns) {
            record.stat = FileStat::UNTRUSTED;
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path, record);
    }

    /// Merge this run's records into the file on disk and replace it atomically.
    /// Does nothing when no record was added.
    pub fn flush(&self) -> anyhow::Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        if pending.is_empty() {
            return Ok(());
        }
        if fingerprint(file) == self.seen {
            write_records(file, [&self.loaded, &pending].into_iter().flatten(), &pending)
        } else {
            let on_disk = read_records(file);
            write_records(file, [&on_disk, &pending].into_iter().flatten(), &pending)
        }
    }
}

fn read_records(file: &Path) -> HashMap<PathKey, Record> {
    let Ok(bytes) = std::fs::read(file) else {
        return HashMap::new();
    };
    let Some(mut rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return HashMap::new();
    };
    let mut records = HashMap::new();
    while !rest.is_empty() {
        let Some((path, record, after)) = decode(rest) else {
            return HashMap::new();
        };
        records.insert(path, record);
        rest = after;
    }
    records
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(len)?;
        self.0 = tail;
        Some(head)
    }

    fn id(&mut self) -> Option<Id> {
        self.take(ID_LEN)?.try_into().ok()
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
}

fn decode(bytes: &[u8]) -> Option<(PathKey, Record, &[u8])> {
    let mut cursor = Cursor(bytes);
    let path = PathKey(cursor.id()?);
    let scope = PlanKey(cursor.id()?);
    let digest = ContentDigest(cursor.id()?);
    let stat = FileStat {
        size: cursor.u64()?,
        mtime_ns: cursor.u64()? as i64,
        ctime_ns: cursor.u64()? as i64,
        ino: cursor.u64()?,
    };
    let verdict = Verdict::from_byte(cursor.u8()?)?;
    let replayable = cursor.u8()? != 0;
    let count = usize::from(cursor.u16()?);
    let mut segments = Vec::with_capacity(count);
    for _ in 0..count {
        let key = PlanKey(cursor.id()?);
        let len = cursor.u32()? as usize;
        segments.push(Segment {
            key,
            payload: cursor.take(len)?.to_vec(),
        });
    }
    let record = Record {
        stat,
        scope,
        digest,
        verdict,
        replayable,
        segments,
    };
    Some((path, record, cursor.0))
}

fn encode(out: &mut Vec<u8>, path: &PathKey, record: &Record) -> Option<()> {
    out.extend_from_slice(&path.0);
    out.extend_from_slice(&record.scope.0);
    out.extend_from_slice(&record.digest.0);
    out.extend_from_slice(&record.stat.size.to_le_bytes());
    out.extend_from_slice(&record.stat.mtime_ns.to_le_bytes());
    out.extend_from_slice(&record.stat.ctime_ns.to_le_bytes());
    out.extend_from_slice(&record.stat.ino.to_le_bytes());
    out.push(record.verdict.to_byte());
    out.push(u8::from(record.replayable));
    out.extend_from_slice(&u16::try_from(record.segments.len()).ok()?.to_le_bytes());
    for segment in &record.segments {
        out.extend_from_slice(&segment.key.0);
        out.extend_from_slice(&u32::try_from(segment.payload.len()).ok()?.to_le_bytes());
        out.extend_from_slice(&segment.payload);
    }
    Some(())
}

/// Write `records` sorted by path key, so the file's bytes depend only on its
/// contents. `overrides` wins over a same-path entry in `records`, which may
/// list a path twice (once stale, once from this run).
fn write_records<'a>(
    file: &Path,
    records: impl Iterator<Item = (&'a PathKey, &'a Record)>,
    overrides: &HashMap<PathKey, Record>,
) -> anyhow::Result<()> {
    let mut ordered: Vec<(&PathKey, &Record)> = records
        .filter(|(path, record)| overrides.get(path).is_none_or(|fresh| std::ptr::eq(fresh, *record)))
        .collect();
    ordered.sort_unstable_by_key(|(path, _)| path.0);
    let mut out = Vec::with_capacity(HEADER_LEN + ordered.len() * 200);
    out.extend_from_slice(MAGIC);
    for (path, record) in ordered {
        if encode(&mut out, path, record).is_none() {
            tracing::debug!("a record exceeded the index encoding limits and was dropped");
        }
    }
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    crate::permissions::create_dir_all_private(parent)
        .map_err(|e| anyhow::anyhow!("failed to create index dir {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        file.file_name().and_then(|n| n.to_str()).unwrap_or("index"),
        std::process::id()
    ));
    std::fs::write(&tmp, &out).map_err(|e| anyhow::anyhow!("index write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, file).map_err(|e| anyhow::anyhow!("index rename to {}: {e}", file.display()))
}

#[cfg(test)]
mod tests;
