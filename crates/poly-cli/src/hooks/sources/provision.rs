//! The provisioning pipeline: resolve every declared source (local path or
//! pinned Git checkout), load its catalog, and select hooks from it.

use std::path::{Path, PathBuf};

use anyhow::Context;
use poly_config::{HookSource, HooksConfig, load_hook_preferences};

use super::lock::lock_source;
use super::manifest::{load_manifest, reject_legacy_consumer_file};
use super::select::{ResolvedHook, select_hooks};
use crate::polylock::{self, LockedHookSource, PolyLock};
use crate::remote::{
    checkout_is_valid, ensure_commit, ensure_mirror, make_read_only, materialize_checkout, resolve_remote_revision,
    validate_locked_revision,
};

/// Directories to search for `poly.local.toml`, nearest first.
///
/// `poly.local.toml` is gitignored by design, so it is never present in a freshly
/// created linked worktree — while the hooks themselves *are*, because they live
/// in the shared `.git/hooks`. Searching the worktree root alone therefore made
/// every new worktree fail to provision (issue #15); the main worktree root is
/// where the machine's real preferences live.
fn preference_roots(root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![root.to_path_buf()];
    if let Some(main) = main_worktree_root()
        && main != root
    {
        roots.push(main);
    }
    roots
}

/// The directory holding the primary `.git` — the same as the worktree root for
/// an ordinary checkout, and the main worktree for a linked one.
///
/// `--git-common-dir` is relative (`.git`) when git is run from the top level,
/// so it is resolved against the working directory before taking its parent.
fn main_worktree_root() -> Option<PathBuf> {
    let common = poly_hooks::git::get_git_common_dir().ok()?;
    let common = if common.is_absolute() {
        common
    } else {
        std::env::current_dir().ok()?.join(common)
    };
    common.parent().map(Path::to_path_buf)
}

/// Resolve selected sources and choose one eligible path for every selected hook.
///
/// Reads revisions from `poly.lock` only; a Git source with no entry fails with a
/// hint to run `poly lock`.
pub fn provision(root: &Path, hooks: &HooksConfig, install: bool) -> anyhow::Result<Vec<ResolvedHook>> {
    reject_legacy_consumer_file(root)?;
    if hooks.sources.is_empty() {
        return Ok(Vec::new());
    }
    let preferences = load_hook_preferences(&preference_roots(root), true)?;
    let cache_root = poly_cache::hook_sources_dir()?;
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("creating hook source cache {}", cache_root.display()))?;
    let existing = polylock::read(root)?;
    let mut resolved = Vec::new();
    for source in &hooks.sources {
        let (_, source_root) = provision_source(
            root,
            &cache_root,
            source,
            existing.as_ref().map(|lock| lock.hook_sources.as_slice()),
        )?;
        let manifest = load_manifest(&source_root)?;
        resolved.extend(select_hooks(source, &source_root, manifest, &preferences, install)?);
    }
    Ok(resolved)
}

/// The `(id, url)` key of every Git hook source declared in `hooks`, sorted.
pub fn declared_git_keys(hooks: &HooksConfig) -> Vec<(String, String)> {
    let mut keys: Vec<_> = hooks
        .sources
        .iter()
        .filter(|source| source.path.is_none())
        .filter_map(|source| Some((source.id.clone(), source.git.clone()?)))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Resolve every Git hook source in `hooks` to a pinned revision for `poly.lock`.
///
/// Without `upgrade`, a source already recorded in `existing` keeps its entry;
/// with it, each declared revision is re-resolved against the remote. Local
/// `path` sources need no entry.
///
/// A ref is resolved with `git ls-remote` and nothing is downloaded; the checkout
/// is materialized, shallowly, the first time the hooks run. A full object ID is
/// verified by fetching that one commit, so a mistyped one fails here rather
/// than on every later run.
pub fn lock_sources(hooks: &HooksConfig, existing: &PolyLock, upgrade: bool) -> anyhow::Result<Vec<LockedHookSource>> {
    let mut locked = Vec::new();
    let mut cache_root = None;
    for source in hooks.sources.iter().filter(|source| source.path.is_none()) {
        let url = source.git.as_deref().expect("validated Git source");
        if !upgrade && let Some(entry) = existing.hook_source(&source.id, url) {
            locked.push(entry.clone());
            continue;
        }
        if cache_root.is_none() {
            let dir = poly_cache::hook_sources_dir()?;
            std::fs::create_dir_all(&dir).with_context(|| format!("creating hook source cache {}", dir.display()))?;
            cache_root = Some(dir);
        }
        let entry = pin_source(cache_root.as_deref().expect("cache root set above"), source)?;
        println!(
            "{} {} -> {}",
            entry.source,
            source.revision.as_deref().unwrap_or_default(),
            entry.revision
        );
        locked.push(entry);
    }
    Ok(locked)
}

/// Resolve a Git hook source's declared revision to the lock entry pinning it.
///
/// A declared full object ID is not advertised by `ls-remote`, so it is checked
/// by fetching it (shallowly) into the source's cache mirror, unless a valid
/// checkout of it is already cached.
fn pin_source(cache_root: &Path, source: &HookSource) -> anyhow::Result<LockedHookSource> {
    let url = source.git.as_deref().expect("validated Git source");
    let revision = source.revision.as_deref().expect("validated Git revision");
    let context = || format!("resolving hook source {:?} at {revision}", source.id);
    let resolved = resolve_remote_revision(url, revision, cache_root).with_context(context)?;
    let source_key = poly_cache::hook_source_key(url);
    if resolved == revision {
        let source_cache = cache_root.join(&source_key);
        let _guard = lock_source(&source_cache)?;
        if !checkout_is_valid(&source_cache.join("checkouts").join(&resolved), &resolved) {
            let mirror = source_cache.join("mirror.git");
            ensure_mirror(&mirror, url)
                .and_then(|()| ensure_commit(&mirror, url, &resolved))
                .with_context(context)?;
        }
    }
    Ok(LockedHookSource {
        id: source.id.clone(),
        source: url.to_string(),
        path: format!("cache://hook-sources/{source_key}/{resolved}"),
        revision: resolved,
    })
}

fn provision_source(
    root: &Path,
    cache_root: &Path,
    source: &HookSource,
    existing: Option<&[LockedHookSource]>,
) -> anyhow::Result<(Option<LockedHookSource>, PathBuf)> {
    if let Some(path) = &source.path {
        let candidate = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("resolving local hook source {}", candidate.display()))?;
        return Ok((None, canonical));
    }
    let url = source.git.as_deref().expect("validated Git source");
    let source_key = poly_cache::hook_source_key(url);
    let source_cache = cache_root.join(&source_key);
    let mirror = source_cache.join("mirror.git");
    let locked = existing.and_then(|entries| {
        entries
            .iter()
            .find(|entry| entry.id == source.id && entry.source == url)
    });
    let locked = locked.ok_or_else(|| {
        anyhow::anyhow!(
            "Git hook source {:?} has no lock entry; run `poly lock` first{}",
            source.id,
            polylock::legacy_hint(root)
        )
    })?;
    validate_locked_revision(&locked.revision)?;
    let checkout = source_cache.join("checkouts").join(&locked.revision);
    let _guard = lock_source(&source_cache)?;
    if checkout_is_valid(&checkout, &locked.revision) {
        make_read_only(&checkout)?;
        return Ok((Some(locked.clone()), checkout));
    }
    ensure_mirror(&mirror, url)?;
    ensure_commit(&mirror, url, &locked.revision)?;
    materialize_source_checkout(&mirror, &checkout, &locked.revision, &source.id)?;
    Ok((Some(locked.clone()), checkout))
}

/// Materialize a hook source's checkout, naming poly's own state on failure.
///
/// A git failure here is reported by git *about the cached repository*, so on
/// its own the message describes files in a repository the reader never named.
/// The context pins the failure to the hook source, revision, and cache path it
/// actually belongs to.
fn materialize_source_checkout(mirror: &Path, checkout: &Path, revision: &str, source_id: &str) -> anyhow::Result<()> {
    materialize_checkout(mirror, checkout, revision).with_context(|| {
        format!(
            "provisioning hook source {source_id:?} at revision {revision} into {}",
            checkout.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::super::manifest::PRODUCER_MANIFEST_NAME;
    use super::super::test_support::{preferences, write_catalog, write_consumer};
    use super::*;
    use crate::remote::{git_output, run_command, run_git};

    fn git_source(repository: &Path) -> HookSource {
        HookSource {
            id: "rules".to_string(),
            path: None,
            git: Some(repository.to_string_lossy().into_owned()),
            revision: Some("HEAD".to_string()),
            hooks: vec!["validate".to_string()],
        }
    }

    fn create_git_source() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        run_command(
            Command::new("git").arg("init").arg("--quiet").arg(repository.path()),
            "initialize test source",
        )
        .unwrap();
        std::fs::write(repository.path().join(PRODUCER_MANIFEST_NAME), "version=1\nhooks=[]\n").unwrap();
        run_git(repository.path(), &["add", PRODUCER_MANIFEST_NAME]).unwrap();
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
                "catalog",
            ]),
            "commit test source",
        )
        .unwrap();
        repository
    }

    /// Pin `source` the way `poly lock` does, then provision it from that entry.
    fn pin_and_provision(cache_root: &Path, source: &HookSource) -> (Option<LockedHookSource>, PathBuf) {
        let entry = pin_source(cache_root, source).unwrap();
        provision_source(Path::new("."), cache_root, source, Some(std::slice::from_ref(&entry))).unwrap()
    }

    #[cfg_attr(windows, allow(clippy::permissions_set_readonly_false))]
    fn make_writable(root: &Path) {
        for entry in walkdir::WalkDir::new(root)
            .contents_first(true)
            .into_iter()
            .filter_map(Result::ok)
        {
            let mut permissions = entry.metadata().unwrap().permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            #[cfg(not(unix))]
            permissions.set_readonly(false);
            std::fs::set_permissions(entry.path(), permissions).unwrap();
        }
    }

    #[test]
    fn selects_explicit_hook_and_guarded_path() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        write_catalog(producer.path());
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["validate"]);
        let selected = provision(consumer.path(), &hooks, true).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].command, "printf");
    }

    #[test]
    fn reports_unknown_hook() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        write_catalog(producer.path());
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["missing"]);
        assert!(
            provision(consumer.path(), &hooks, true)
                .unwrap_err()
                .to_string()
                .contains("unknown hook")
        );
    }

    #[test]
    fn installs_selected_path_only_when_requested() {
        let consumer = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(
            producer.path().join(PRODUCER_MANIFEST_NAME),
            r#"
version = 1
[[hooks]]
id = "validate"
stages = ["pre-commit"]
[[hooks.paths]]
channel = "shell"
check = "command -v printf"
install = "printf installed > installed.txt"
run = "printf"
"#,
        )
        .unwrap();
        preferences(consumer.path());
        let hooks = write_consumer(consumer.path(), producer.path(), &["validate"]);
        provision(consumer.path(), &hooks, false).unwrap();
        assert!(!producer.path().join("installed.txt").exists());
        provision(consumer.path(), &hooks, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(producer.path().join("installed.txt")).unwrap(),
            "installed"
        );
    }

    #[test]
    fn rejects_legacy_consumer_catalog() {
        let root = tempfile::tempdir().unwrap();
        let producer = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(PRODUCER_MANIFEST_NAME), "version=1\nsources=[]").unwrap();
        preferences(root.path());
        let hooks = write_consumer(root.path(), producer.path(), &["validate"]);
        assert!(
            provision(root.path(), &hooks, true)
                .unwrap_err()
                .to_string()
                .contains("producer catalog")
        );
    }

    #[test]
    fn concurrent_consumers_share_one_mirror_and_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let cache_root = cache.path().to_path_buf();
            let source = source.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                pin_and_provision(&cache_root, &source)
            }));
        }
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first.1, second.1);
        let source_cache = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()));
        assert!(source_cache.join("mirror.git").is_dir());
        assert_eq!(std::fs::read_dir(source_cache.join("checkouts")).unwrap().count(), 1);
        assert!(
            std::fs::metadata(first.1.join(PRODUCER_MANIFEST_NAME))
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    #[test]
    fn normal_run_replaces_checkout_with_wrong_head() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = pin_and_provision(cache.path(), &source);
        let lock = vec![locked.unwrap()];
        make_writable(&checkout);
        std::fs::write(checkout.join(".git/HEAD"), "0000000000000000000000000000000000000000\n").unwrap();

        let (_, reconstructed) =
            provision_source(Path::new("."), cache.path(), &source, Some(lock.as_slice())).unwrap();

        assert_eq!(reconstructed, checkout);
        assert_eq!(git_output(&checkout, &["rev-parse", "HEAD"]).unwrap(), lock[0].revision);
    }

    #[test]
    fn normal_run_replaces_tampered_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = pin_and_provision(cache.path(), &source);
        let lock = vec![locked.unwrap()];
        make_writable(&checkout);
        std::fs::write(
            checkout.join(PRODUCER_MANIFEST_NAME),
            "version=1\nhooks=[]\n# tampered\n",
        )
        .unwrap();

        provision_source(Path::new("."), cache.path(), &source, Some(lock.as_slice())).unwrap();

        assert!(
            !std::fs::read_to_string(checkout.join(PRODUCER_MANIFEST_NAME))
                .unwrap()
                .contains("tampered")
        );
    }

    #[test]
    fn normal_run_reuses_valid_checkout_without_mirror() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = pin_and_provision(cache.path(), &source);
        let lock = vec![locked.unwrap()];
        let source_cache = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()));
        std::fs::remove_dir_all(source_cache.join("mirror.git")).unwrap();

        let (_, reused) = provision_source(Path::new("."), cache.path(), &source, Some(lock.as_slice())).unwrap();

        assert_eq!(reused, checkout);
        assert!(!source_cache.join("mirror.git").exists());
        assert!(!checkout.join(".git/objects/info/alternates").exists());
    }

    fn commit_in(repository: &Path, message: &str) {
        run_command(
            Command::new("git").arg("-C").arg(repository).args([
                "-c",
                "user.name=Poly Test",
                "-c",
                "user.email=poly@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                message,
            ]),
            "commit test source",
        )
        .unwrap();
    }

    #[test]
    fn pin_resolves_refs_without_downloading_and_verifies_a_full_sha() {
        let producer = create_git_source();
        let tagged = git_output(producer.path(), &["rev-parse", "HEAD"]).unwrap();
        run_command(
            Command::new("git").arg("-C").arg(producer.path()).args([
                "-c",
                "user.name=Poly Test",
                "-c",
                "user.email=poly@example.invalid",
                "-c",
                "tag.gpgSign=false",
                "tag",
                "--annotate",
                "-m",
                "release",
                "v1",
            ]),
            "tag test source",
        )
        .unwrap();
        commit_in(producer.path(), "after the tag");
        let head = git_output(producer.path(), &["rev-parse", "HEAD"]).unwrap();
        let branch = git_output(producer.path(), &["symbolic-ref", "--short", "HEAD"]).unwrap();
        let tag_object = git_output(producer.path(), &["rev-parse", "v1"]).unwrap();
        assert_ne!(tag_object, tagged, "the fixture tag must be annotated");

        let cache = tempfile::tempdir().unwrap();
        let mut source = git_source(producer.path());
        let key = poly_cache::hook_source_key(source.git.as_deref().unwrap());
        for (revision, expected) in [("v1", &tagged), (branch.as_str(), &head)] {
            source.revision = Some(revision.to_string());
            let entry = pin_source(cache.path(), &source).unwrap();
            assert_eq!(&entry.revision, expected, "revision {revision}");
            assert_eq!(entry.path, format!("cache://hook-sources/{key}/{expected}"));
        }
        assert!(
            !cache.path().join(&key).exists(),
            "resolving a ref must not download the source"
        );

        source.revision = Some(tagged.clone());
        assert_eq!(pin_source(cache.path(), &source).unwrap().revision, tagged);
        assert!(cache.path().join(&key).join("mirror.git").is_dir());

        source.revision = Some("no-such-ref".to_string());
        assert!(format!("{:#}", pin_source(cache.path(), &source).unwrap_err()).contains("not found"));
    }

    #[test]
    fn pin_rejects_a_short_sha() {
        let producer = create_git_source();
        let head = git_output(producer.path(), &["rev-parse", "HEAD"]).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut source = git_source(producer.path());
        source.revision = Some(head[..12].to_string());
        let error = format!("{:#}", pin_source(cache.path(), &source).unwrap_err());
        assert!(error.contains("not found"), "{error}");
    }

    #[test]
    fn pin_rejects_a_nonexistent_full_sha_at_lock_time() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let mut source = git_source(producer.path());
        let missing = "0123456789abcdef0123456789abcdef01234567";
        source.revision = Some(missing.to_string());
        let error = format!("{:#}", pin_source(cache.path(), &source).unwrap_err());
        assert!(error.contains("resolving hook source \"rules\""), "{error}");
        assert!(error.contains(missing), "{error}");
        assert!(
            error.contains("shallow fetch"),
            "both fetch failures are reported: {error}"
        );
    }

    #[test]
    fn provisioning_fetches_only_the_pinned_commit() {
        let producer = create_git_source();
        commit_in(producer.path(), "second");
        commit_in(producer.path(), "third");
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = pin_and_provision(cache.path(), &source);
        let revision = locked.unwrap().revision;

        assert_eq!(git_output(&checkout, &["rev-parse", "HEAD"]).unwrap(), revision);
        let mirror = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()))
            .join("mirror.git");
        assert_eq!(
            git_output(&mirror, &["rev-parse", "--is-shallow-repository"]).unwrap(),
            "true"
        );
        assert_eq!(git_output(&mirror, &["rev-list", "--count", "--all"]).unwrap(), "1");
    }

    #[test]
    fn normal_run_rejects_non_oid_lock_revision() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let lock = vec![LockedHookSource {
            id: source.id.clone(),
            source: source.git.clone().unwrap(),
            revision: "../../outside".to_string(),
            path: "cache://invalid".to_string(),
        }];

        let error = provision_source(Path::new("."), cache.path(), &source, Some(lock.as_slice())).unwrap_err();

        assert!(error.to_string().contains("full hexadecimal Git object ID"));
    }
}
