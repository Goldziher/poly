//! Unit tests for the result cache: key derivation, storage, and layout.
//! Extracted from `lib.rs` to keep that file under the 1000-line module cap.

use std::collections::HashSet;

use tempfile::TempDir;

use super::*;

/// Open an enabled cache rooted at an explicit temporary directory, so
/// tests are isolated from the process cwd and any real `.git` tree.
fn cache_at(dir: &TempDir) -> ResultCache {
    let root = dir.path().join("cache");
    ResultCache::open(root, true).expect("open cache")
}

fn empty_args() -> toml::Table {
    toml::Table::new()
}

#[test]
fn get_returns_stored_bytes_on_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("content");
    let key = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
    cache.put(Namespace::Lint, &key, b"stored").unwrap();
    assert_eq!(cache.get(Namespace::Lint, &key).as_deref(), Some(&b"stored"[..]));
}

#[test]
fn miss_when_content_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let d1 = ResultCache::single_file_digest("content");
    let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &d1);
    cache.put(Namespace::Lint, &key1, b"stored").unwrap();
    let d2 = ResultCache::single_file_digest("different content");
    let key2 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &d2);
    assert_ne!(key1, key2, "content change must alter key");
    assert_eq!(cache.get(Namespace::Lint, &key2), None);
}

#[test]
fn miss_when_version_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("content");
    let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
    cache.put(Namespace::Lint, &key1, b"stored").unwrap();
    let key2 = ResultCache::key(Namespace::Lint, "eng", "2", &empty_args(), &digest);
    assert_ne!(key1, key2, "version change must alter key");
    assert_eq!(cache.get(Namespace::Lint, &key2), None);
}

#[test]
fn miss_when_args_change() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("content");
    let args_a = empty_args();
    let mut args_b = empty_args();
    args_b.insert("line-length".into(), toml::Value::Integer(120));
    let key1 = ResultCache::key(Namespace::Lint, "eng", "1", &args_a, &digest);
    cache.put(Namespace::Lint, &key1, b"stored").unwrap();
    let key2 = ResultCache::key(Namespace::Lint, "eng", "1", &args_b, &digest);
    assert_ne!(key1, key2, "args change must alter key");
    assert_eq!(cache.get(Namespace::Lint, &key2), None);
}

#[test]
fn disabled_cache_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    let cache = ResultCache::open(root.clone(), false).unwrap();
    let digest = ResultCache::single_file_digest("content");
    let key = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);
    cache.put(Namespace::Lint, &key, b"stored").unwrap();
    assert_eq!(cache.get(Namespace::Lint, &key), None, "disabled get must miss");
    assert!(!root.exists(), "disabled put must not create cache dir");
}

#[test]
fn key_with_pre_serialized_args_matches_key() {
    let digest = ResultCache::single_file_digest("content");
    let mut args = empty_args();
    args.insert("line-length".into(), toml::Value::Integer(120));
    let direct = ResultCache::key(Namespace::Fmt, "eng", "1", &args, &digest);
    let serialized = ResultCache::serialize_args(&args);
    let via_args = ResultCache::key_with_args(Namespace::Fmt, "eng", "1", &serialized, &digest);
    assert_eq!(
        direct, via_args,
        "key and key_with_args(serialize_args(..)) must be byte-identical"
    );
}

#[test]
fn namespace_segregates_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("x");
    let args = empty_args();
    let lint_key = ResultCache::key(Namespace::Lint, "eng", "1", &args, &digest);
    let fmt_key = ResultCache::key(Namespace::Fmt, "eng", "1", &args, &digest);
    let hook_key = ResultCache::key(Namespace::Hook, "eng", "1", &args, &digest);
    let keys: HashSet<_> = [lint_key.as_str(), fmt_key.as_str(), hook_key.as_str()]
        .into_iter()
        .collect();
    assert_eq!(keys.len(), 3, "each namespace must produce a distinct key");
    cache.put(Namespace::Lint, &lint_key, b"lint").unwrap();
    assert_eq!(cache.get(Namespace::Fmt, &fmt_key), None);
    assert_eq!(cache.get(Namespace::Hook, &hook_key), None);
}

#[test]
fn single_file_digest_matches_file_set_of_one_with_empty_path() {
    let content = "hello world";
    let single = ResultCache::single_file_digest(content);
    let set = ResultCache::file_set_digest(std::iter::once(("", content.as_bytes())));
    assert_eq!(
        single, set,
        "single_file_digest must equal file_set_digest({{'', content}})"
    );
}

#[test]
fn file_set_digest_is_path_order_stable() {
    let a = ("alpha.py", b"content_a" as &[u8]);
    let b = ("beta.py", b"content_b" as &[u8]);
    let forward = ResultCache::file_set_digest([a, b].into_iter());
    let backward = ResultCache::file_set_digest([b, a].into_iter());
    assert_eq!(forward, backward, "file_set_digest must be stable across input order");
}

#[test]
fn file_set_digest_differs_on_content_change() {
    let d1 = ResultCache::file_set_digest([("a.py", b"v1" as &[u8]), ("b.py", b"v2")].into_iter());
    let d2 = ResultCache::file_set_digest([("a.py", b"v1" as &[u8]), ("b.py", b"CHANGED")].into_iter());
    assert_ne!(d1, d2);
}

#[test]
fn file_set_digest_is_deterministic_across_calls() {
    let files = || [("a.py", b"alpha" as &[u8]), ("b.py", b"beta")].into_iter();
    assert_eq!(
        ResultCache::file_set_digest(files()),
        ResultCache::file_set_digest(files()),
        "identical input sets must produce identical digests"
    );
}

#[test]
fn file_set_digest_differs_on_path_change() {
    let d1 = ResultCache::file_set_digest(std::iter::once(("a.py", b"same" as &[u8])));
    let d2 = ResultCache::file_set_digest(std::iter::once(("b.py", b"same" as &[u8])));
    assert_ne!(d1, d2);
}

#[test]
fn find_anchor_returns_nearest_ancestor_with_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let deep = root.join("a").join("b");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(find_anchor(&deep, ".git").as_deref(), Some(root));
}

#[test]
fn find_anchor_returns_none_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("x").join("y");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(find_anchor(&deep, ".git"), None);
}

#[test]
fn repo_anchor_prefers_git_over_poly_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("poly.toml"), b"").unwrap();
    assert_eq!(repo_anchor(&pkg), root, ".git anchor must win over poly.toml");
}

#[test]
fn repo_anchor_falls_back_to_poly_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("poly.toml"), b"").unwrap();
    let deep = root.join("sub");
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(repo_anchor(&deep), root);
}

#[test]
fn cache_root_lives_under_cache_home_not_in_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let pkg = root.join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();

    let cache_root = root_from(&pkg).expect("root_from");
    let expected = cache_home().unwrap().join(repo_key(root));
    assert_eq!(cache_root, expected);
    assert!(
        !cache_root.starts_with(root),
        "cache must live outside the repo, got {}",
        cache_root.display()
    );
}

#[test]
fn repo_key_is_stable_and_path_dependent() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("repo-a");
    let b = tmp.path().join("repo-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    assert_eq!(repo_key(&a), repo_key(&a), "same path → same key");
    assert_ne!(repo_key(&a), repo_key(&b), "different path → different key");
    assert_eq!(repo_key(&a).len(), 16, "key is 16 hex chars");
}

#[test]
fn remove_legacy_cache_deletes_dot_polylint() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let legacy = root.join(".polylint").join("cache").join("results");
    std::fs::create_dir_all(&legacy).unwrap();
    assert!(root.join(".polylint").exists());
    remove_legacy_cache(root);
    assert!(!root.join(".polylint").exists(), "legacy .polylint must be removed");
    remove_legacy_cache(root);
}

#[test]
fn version_sentinel_is_written_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    ResultCache::open(root.clone(), true).unwrap();
    let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
    assert_eq!(version, CACHE_FORMAT_VERSION);
}

#[test]
fn open_healed_wipes_when_sentinel_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    std::fs::create_dir_all(root.join("results/lint")).unwrap();
    std::fs::create_dir_all(root.join("results/fmt")).unwrap();
    std::fs::create_dir_all(root.join("results/hook")).unwrap();
    std::fs::write(root.join("results/lint/stale-entry"), b"cached").unwrap();
    std::fs::write(root.join("VERSION"), "0").unwrap();

    ResultCache::open_healed(root.clone(), true).unwrap();

    assert!(
        !root.join("results/lint/stale-entry").exists(),
        "an incompatible-layout entry must be wiped on a run-path open"
    );
    let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
    assert_eq!(
        version, CACHE_FORMAT_VERSION,
        "the sentinel must be rewritten to the current format version"
    );
}

#[test]
fn open_healed_preserves_entries_when_sentinel_current() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    std::fs::create_dir_all(root.join("results/lint")).unwrap();
    std::fs::create_dir_all(root.join("results/fmt")).unwrap();
    std::fs::create_dir_all(root.join("results/hook")).unwrap();
    std::fs::write(root.join("results/lint/entry"), b"cached").unwrap();
    std::fs::write(root.join("VERSION"), CACHE_FORMAT_VERSION).unwrap();

    ResultCache::open_healed(root.clone(), true).unwrap();

    assert!(
        root.join("results/lint/entry").exists(),
        "a current-layout entry must survive a run-path open"
    );
}

#[test]
fn open_does_not_wipe_a_stale_layout() {
    // The maintenance open is read-only: `poly cache stats`/`size` must never
    // wipe, so a stale sentinel and its entries survive a plain `open`.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("cache");
    std::fs::create_dir_all(root.join("results/lint")).unwrap();
    std::fs::create_dir_all(root.join("results/fmt")).unwrap();
    std::fs::create_dir_all(root.join("results/hook")).unwrap();
    std::fs::write(root.join("results/lint/entry"), b"cached").unwrap();
    std::fs::write(root.join("VERSION"), "0").unwrap();

    ResultCache::open(root.clone(), true).unwrap();

    assert!(
        root.join("results/lint/entry").exists(),
        "a plain maintenance open must not wipe a stale-layout tree"
    );
    let version = std::fs::read_to_string(root.join("VERSION")).unwrap();
    assert_eq!(version, "0", "a plain open must not rewrite the sentinel");
}

#[test]
fn key_folds_in_the_build_identity() {
    let digest = ResultCache::single_file_digest("content");
    let args = ResultCache::serialize_args(&empty_args());
    let older = ResultCache::key_with_build_identity("release/0.0.1", Namespace::Lint, "eng", "1", &args, &digest);
    let newer = ResultCache::key_with_build_identity("release/0.0.2", Namespace::Lint, "eng", "1", &args, &digest);
    assert_ne!(older, newer, "a poly version change must alter the key");
}

/// The regression this key component exists for: two builds that report the
/// same version but behave differently must not read each other's entries.
#[test]
fn a_different_build_of_one_version_misses_the_other_builds_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("already formatted\n");
    let args = ResultCache::serialize_args(&empty_args());

    let released = ResultCache::key_with_build_identity("release/0.19.7", Namespace::Fmt, "eng", "1", &args, &digest);
    cache.put(Namespace::Fmt, &released, b"already formatted\n").unwrap();

    let unreleased = ResultCache::key_with_build_identity(
        "dev/0.19.7/v0.19.7-11-g714d4b9/release/900-1",
        Namespace::Fmt,
        "eng",
        "1",
        &args,
        &digest,
    );
    assert_ne!(released, unreleased, "an unreleased 0.19.7 keys differently");
    assert_eq!(
        cache.get(Namespace::Fmt, &unreleased),
        None,
        "an unreleased build must not be served the released build's verdict"
    );
}

/// The other half of the trade: identical builds must still share, or the
/// cache stops paying for itself across machines and CI runs.
#[test]
fn identical_builds_share_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_at(&tmp);
    let digest = ResultCache::single_file_digest("content");
    let args = ResultCache::serialize_args(&empty_args());
    let first = ResultCache::key_with_build_identity("release/0.19.7", Namespace::Lint, "eng", "1", &args, &digest);
    let second = ResultCache::key_with_build_identity("release/0.19.7", Namespace::Lint, "eng", "1", &args, &digest);
    cache.put(Namespace::Lint, &first, b"stored").unwrap();
    assert_eq!(first, second);
    assert_eq!(cache.get(Namespace::Lint, &second).as_deref(), Some(&b"stored"[..]));
}

#[test]
fn the_public_key_path_uses_this_binarys_build_identity() {
    let digest = ResultCache::single_file_digest("content");
    let args = ResultCache::serialize_args(&empty_args());
    let public = ResultCache::key_with_args(Namespace::Lint, "eng", "1", &args, &digest);
    let explicit = ResultCache::key_with_build_identity(
        poly_buildinfo::cache_identity(),
        Namespace::Lint,
        "eng",
        "1",
        &args,
        &digest,
    );
    assert_eq!(public, explicit, "the public path must fold in this build's identity");
}

#[test]
fn hook_sources_are_global_and_url_keyed() {
    let root = tempfile::tempdir().unwrap();
    let first_repo = root.path().join("first");
    let second_repo = root.path().join("second");
    std::fs::create_dir_all(&first_repo).unwrap();
    std::fs::create_dir_all(&second_repo).unwrap();

    let global = hook_sources_dir().unwrap();
    assert_eq!(global, cache_home().unwrap().join("hook-sources"));
    assert!(!global.starts_with(repo_cache_dir(&first_repo).unwrap()));
    assert!(!global.starts_with(repo_cache_dir(&second_repo).unwrap()));
    assert_ne!(
        hook_source_key("https://example.com/hooks.git/"),
        hook_source_key("https://example.com/hooks.git")
    );
    assert_ne!(
        hook_source_key("https://example.com/one"),
        hook_source_key("https://example.com/two")
    );
}
