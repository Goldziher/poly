use std::path::Path;

use tempfile::TempDir;

use super::{ContentDigest, FileStat, PathKey, PlanKey, RACY_WINDOW_NS, Record, Segment, StatIndex, Verdict};
use crate::Namespace;

const HOUR_NS: i64 = 3_600 * 1_000_000_000;
const NOW: i64 = 10 * HOUR_NS;

fn old_stat() -> FileStat {
    FileStat::synthetic(10, NOW - 2 * HOUR_NS, NOW - 2 * HOUR_NS, 7)
}

fn key(tag: &str) -> PlanKey {
    PlanKey::builder(Namespace::Lint).part(tag).finish()
}

fn path(name: &str) -> PathKey {
    PathKey::of(Path::new(name))
}

fn record(stat: FileStat, bytes: &[u8]) -> Record {
    Record {
        stat,
        scope: key("scope"),
        digest: ContentDigest::of(bytes),
        verdict: Verdict::Clean,
        replayable: true,
        segments: vec![Segment {
            key: key("ruff"),
            payload: Vec::new(),
        }],
    }
}

fn index_in(dir: &TempDir) -> StatIndex {
    StatIndex::load(dir.path().join("index").join("lint.idx"))
}

fn put(index: &StatIndex, name: &str, record: Record) {
    index.put_at(NOW, path(name), record);
}

#[test]
fn a_record_survives_a_flush_and_reload_intact() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_in(&dir);
    let mut original = record(old_stat(), b"x = 1\n");
    original.verdict = Verdict::Diagnostics;
    original.replayable = false;
    original.segments = vec![
        Segment {
            key: key("ruff"),
            payload: b"[{\"a\":1}]".to_vec(),
        },
        Segment {
            key: key("typos"),
            payload: Vec::new(),
        },
    ];
    put(&index, "a.py", original.clone());
    index.flush().unwrap();

    assert_eq!(index_in(&dir).get(&path("a.py")), Some(&original));
}

#[test]
fn every_stat_field_is_compared() {
    let stored = record(old_stat(), b"x");
    let base = old_stat();
    assert!(stored.trusts(&base));
    let variants = [
        FileStat::synthetic(11, base.mtime_ns, base.ctime_ns, base.ino),
        FileStat::synthetic(base.size, base.mtime_ns + 1, base.ctime_ns, base.ino),
        FileStat::synthetic(base.size, base.mtime_ns, base.ctime_ns + 1, base.ino),
        FileStat::synthetic(base.size, base.mtime_ns, base.ctime_ns, base.ino + 1),
    ];
    for variant in variants {
        assert!(!stored.trusts(&variant), "{variant:?}");
    }
}

#[test]
fn engine_keys_must_match_in_order_and_count() {
    let stored = record(old_stat(), b"x");
    assert!(stored.has_keys(&[key("ruff")]));
    assert!(!stored.has_keys(&[key("other")]));
    assert!(!stored.has_keys(&[]));
    assert!(!stored.has_keys(&[key("ruff"), key("typos")]));
}

#[test]
fn plan_parts_are_length_delimited_and_namespaced() {
    let split_early = PlanKey::builder(Namespace::Lint).part("ab").part("c").finish();
    let split_late = PlanKey::builder(Namespace::Lint).part("a").part("bc").finish();
    assert_ne!(split_early, split_late);
    assert_ne!(
        PlanKey::builder(Namespace::Lint).part("x").finish(),
        PlanKey::builder(Namespace::Fmt).part("x").finish(),
    );
    assert_ne!(key("p"), key("p").extended(["ctx"]));
    assert_ne!(key("p").extended(["a"]), key("p").extended(["b"]));
}

#[test]
fn a_file_modified_inside_the_racy_window_is_kept_but_never_trusted_by_stat() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_in(&dir);
    let fresh = FileStat::synthetic(10, NOW - RACY_WINDOW_NS + 1, NOW - 2 * HOUR_NS, 7);
    put(&index, "a.py", record(fresh, b"x"));
    index.flush().unwrap();

    let stored = index_in(&dir).get(&path("a.py")).cloned().unwrap();
    assert!(!stored.trusts(&fresh));
    assert_eq!(
        stored.digest,
        ContentDigest::of(b"x"),
        "the content check can still vouch for it"
    );
}

#[test]
fn a_file_whose_ctime_is_inside_the_racy_window_is_not_trusted_either() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_in(&dir);
    let renamed_in = FileStat::synthetic(10, NOW - 2 * HOUR_NS, NOW - 1, 7);
    put(&index, "a.py", record(renamed_in, b"x"));
    index.flush().unwrap();

    assert!(!index_in(&dir).get(&path("a.py")).unwrap().trusts(&renamed_in));
}

#[test]
fn a_file_just_outside_the_racy_window_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_in(&dir);
    let settled = FileStat::synthetic(10, NOW - RACY_WINDOW_NS, NOW - RACY_WINDOW_NS, 7);
    put(&index, "a.py", record(settled, b"x"));
    index.flush().unwrap();

    assert!(index_in(&dir).get(&path("a.py")).unwrap().trusts(&settled));
}

#[test]
fn the_untrusted_stat_matches_no_real_file() {
    assert!(!record(FileStat::UNTRUSTED, b"x").trusts(&old_stat()));
    assert!(!record(FileStat::UNTRUSTED, b"x").trusts(&FileStat::synthetic(0, 0, 0, 0)));
}

#[test]
fn flush_merges_records_written_by_a_concurrent_run() {
    let dir = tempfile::tempdir().unwrap();
    let first = index_in(&dir);
    let second = index_in(&dir);
    put(&first, "a.py", record(old_stat(), b"a"));
    put(&second, "b.py", record(old_stat(), b"b"));
    first.flush().unwrap();
    second.flush().unwrap();

    let warm = index_in(&dir);
    assert!(warm.get(&path("a.py")).is_some());
    assert!(warm.get(&path("b.py")).is_some());
}

#[test]
fn a_newer_record_replaces_the_loaded_one_for_the_same_path() {
    let dir = tempfile::tempdir().unwrap();
    let first = index_in(&dir);
    put(&first, "a.py", record(old_stat(), b"old"));
    first.flush().unwrap();

    let second = index_in(&dir);
    put(&second, "a.py", record(old_stat(), b"new"));
    second.flush().unwrap();

    assert_eq!(
        index_in(&dir).get(&path("a.py")).unwrap().digest,
        ContentDigest::of(b"new")
    );
}

#[test]
fn a_corrupt_or_foreign_file_loads_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lint.idx");
    let index = index_in(&dir);
    put(&index, "a.py", record(old_stat(), b"x"));
    index.flush().unwrap();
    let mut truncated = std::fs::read(dir.path().join("index").join("lint.idx")).unwrap();
    truncated.truncate(truncated.len() - 3);

    for bytes in [&b"garbage"[..], b"PSI2\x01\x02", b"", &truncated] {
        std::fs::write(&file, bytes).unwrap();
        assert!(StatIndex::load(file.clone()).get(&path("a.py")).is_none());
    }
}

#[test]
fn a_disabled_index_neither_records_nor_hits() {
    let index = StatIndex::disabled();
    put(&index, "a.py", record(old_stat(), b"x"));
    index.flush().unwrap();
    assert!(index.get(&path("a.py")).is_none());
}

#[test]
fn the_index_file_is_byte_stable_across_flushes() {
    let dir = tempfile::tempdir().unwrap();
    let index = index_in(&dir);
    for name in ["c.py", "a.py", "b.py"] {
        put(&index, name, record(old_stat(), name.as_bytes()));
    }
    index.flush().unwrap();
    let file = dir.path().join("index").join("lint.idx");
    let first = std::fs::read(&file).unwrap();

    let again = index_in(&dir);
    put(&again, "a.py", record(old_stat(), b"a.py"));
    again.flush().unwrap();

    assert_eq!(std::fs::read(&file).unwrap(), first);
}
