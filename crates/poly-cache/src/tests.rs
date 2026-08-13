//! Unit tests for the result cache: key derivation, storage, and layout.
//! Extracted from `lib.rs` to keep that file under the 1000-line module cap.

use std::collections::HashSet;

use tempfile::TempDir;

use super::*;

/// The per-repo slot [`repo_cache_dir`] creates in the *real* per-user cache
/// home for a temporary repository, removed again when the test ends.
///
/// `repo_cache_dir` creates the directory it resolves (that is what keeps the
/// staged snapshot owner-only), and the tests below deliberately exercise the
/// un-overridden cache home. The key is derived from a unique temp path, so the
/// slot belongs to exactly one test and cleaning it up is race-free.
struct RealCacheSlot {
    dir: PathBuf,
}

impl RealCacheSlot {
    fn of(repo: &Path) -> Self {
        Self {
            dir: repo_cache_dir(repo).expect("resolve cache dir"),
        }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for RealCacheSlot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Run `action` and return the fields of every `WARN` event it emitted.
///
/// The permission policy's whole point is *which* situations warn, so "warned"
/// and "stayed silent" have to be assertable. The subscriber is installed
/// thread-locally via [`tracing::subscriber::with_default`], so a capturing test
/// never steals events from — or loses events to — tests running in parallel.
#[cfg(unix)]
pub(crate) fn warnings_during(action: impl FnOnce()) -> Vec<String> {
    use std::fmt::Write;
    use std::sync::{Arc, Mutex};

    /// Flattens an event's fields (message included) into one searchable line.
    #[derive(Default)]
    struct Fields(String);

    impl tracing::field::Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }

    struct Capture(Arc<Mutex<Vec<String>>>);

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().expect("warning sink").push(fields.0);
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    let sink = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(Capture(Arc::clone(&sink)), action);
    sink.lock().expect("warning sink").clone()
}

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

    ResultCache::open_healed(root.clone(), true, DirOrigin::UserConfigured).unwrap();

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

    ResultCache::open_healed(root.clone(), true, DirOrigin::UserConfigured).unwrap();

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

    let first_slot = RealCacheSlot::of(&first_repo);
    let second_slot = RealCacheSlot::of(&second_repo);

    let global = hook_sources_dir().unwrap();
    assert_eq!(global, cache_home().unwrap().join("hook-sources"));
    assert!(!global.starts_with(first_slot.path()));
    assert!(!global.starts_with(second_slot.path()));
    assert_ne!(
        hook_source_key("https://example.com/hooks.git/"),
        hook_source_key("https://example.com/hooks.git")
    );
    assert_ne!(
        hook_source_key("https://example.com/one"),
        hook_source_key("https://example.com/two")
    );
}

#[cfg(unix)]
mod unix_permissions {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::*;

    const OWNER_ONLY: u32 = 0o700;

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    }

    #[test]
    fn open_creates_the_cache_tree_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");

        ResultCache::open(root.clone(), true).expect("open cache");

        assert_eq!(mode_of(&root), OWNER_ONLY, "the cache root must be owner-only");
        for sub in ["results", "results/lint", "results/fmt", "results/hook"] {
            assert_eq!(mode_of(&root.join(sub)), OWNER_ONLY, "{sub} must be created owner-only");
        }
    }

    #[test]
    fn cache_still_stores_and_reads_entries_with_owner_only_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = cache_at(&tmp);
        let digest = ResultCache::single_file_digest("content");
        let key = ResultCache::key(Namespace::Lint, "eng", "1", &empty_args(), &digest);

        cache.put(Namespace::Lint, &key, b"stored").unwrap();

        assert_eq!(cache.get(Namespace::Lint, &key).as_deref(), Some(&b"stored"[..]));
        assert_eq!(mode_of(cache.root()), OWNER_ONLY, "hardening must not break get/put");
    }

    #[test]
    fn repo_cache_dir_creates_an_owner_only_slot_for_the_staged_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let slot = RealCacheSlot::of(&repo);

        assert!(slot.path().is_dir(), "the per-repo slot must exist after resolution");
        assert_eq!(
            mode_of(slot.path()),
            OWNER_ONLY,
            "the slot holding the staged snapshot must not be traversable by other users"
        );

        // The staged snapshot is created by poly-hooks with a plain
        // `create_dir_all`; an owner-only parent is what seals it, since Unix
        // checks every path component.
        let staged = slot.path().join("staged");
        std::fs::create_dir_all(&staged).unwrap();
        assert_eq!(mode_of(slot.path()), OWNER_ONLY, "the parent stays owner-only");
    }

    #[test]
    fn disabled_open_from_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let expected = cache_home().unwrap().join(repo_key(&repo));

        let cache = ResultCache::open_from(&repo, false).expect("open disabled");

        assert_eq!(cache.root(), expected);
        assert!(!expected.exists(), "a disabled cache must not materialize a directory");
    }

    /// An explicit root is `[cache] dir` / `--cache-dir`: the user named the
    /// location, so its mode may be a deliberate choice and poly only reports it.
    #[test]
    fn an_existing_loose_directory_at_a_configured_root_keeps_its_mode_and_warns() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("shared-cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        let warnings = warnings_during(|| {
            ResultCache::open(root.clone(), true).expect("open cache");
        });

        assert_eq!(
            mode_of(&root),
            0o755,
            "a pre-existing (possibly deliberately shared) cache dir must not be silently tightened"
        );
        assert_eq!(
            mode_of(&root.join("results/lint")),
            OWNER_ONLY,
            "sub-directories poly creates itself are still owner-only"
        );
        assert_eq!(
            warnings.len(),
            1,
            "the user must be told, since only they can decide: {warnings:?}"
        );
        assert!(
            warnings[0].contains(&root.display().to_string()),
            "the warning must name the directory: {}",
            warnings[0]
        );
    }

    /// The upgrade regression this split exists for: every slot an older poly
    /// created in the default per-user cache home is `0755`, and 0.20.0 warned
    /// about it on every single run forever. poly chose that location and
    /// created it, so it repairs it instead of nagging.
    #[test]
    fn a_loose_slot_in_the_default_cache_home_is_tightened_silently() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let slot = RealCacheSlot::of(&repo);
        std::fs::set_permissions(slot.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let warnings = warnings_during(|| {
            repo_cache_dir(&repo).expect("resolve cache dir");
        });

        assert_eq!(
            mode_of(slot.path()),
            OWNER_ONLY,
            "a slot poly created in its own cache home must be tightened on the next run"
        );
        assert!(
            warnings.is_empty(),
            "there is nothing for the user to decide, so nothing to warn about: {warnings:?}"
        );
    }

    /// The same repair reaches the run-path open, not just the bare resolver.
    #[test]
    fn opening_a_cache_in_the_default_cache_home_tightens_a_loose_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let slot = RealCacheSlot::of(&repo);
        std::fs::set_permissions(slot.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        let warnings = warnings_during(|| {
            ResultCache::open_from(&repo, true).expect("open cache");
        });

        assert_eq!(
            mode_of(slot.path()),
            OWNER_ONLY,
            "the cache root must end up owner-only"
        );
        assert!(
            warnings.is_empty(),
            "no warning on a directory poly fixed itself: {warnings:?}"
        );
    }
}
