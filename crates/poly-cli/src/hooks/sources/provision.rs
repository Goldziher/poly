//! The provisioning pipeline: resolve every declared source (local path or
//! pinned Git checkout), load its catalog, and select hooks from it.

use std::path::{Path, PathBuf};

use anyhow::Context;
use poly_config::{HookSource, HooksConfig, load_hook_preferences};

use super::lock::{HookSourceLock, LockedSource, lock_source, read_lock, remove_lock, write_lock};
use super::manifest::{load_manifest, reject_legacy_consumer_file};
use super::select::{ResolvedHook, select_hooks};
use crate::remote::{
    checkout_is_valid, ensure_commit, ensure_mirror, git_output, make_read_only, materialize_checkout, run_git,
    validate_locked_revision,
};

/// Resolve selected sources and choose one eligible path for every selected hook.
pub fn provision(root: &Path, hooks: &HooksConfig, update: bool, install: bool) -> anyhow::Result<Vec<ResolvedHook>> {
    reject_legacy_consumer_file(root)?;
    if hooks.sources.is_empty() {
        return Ok(Vec::new());
    }
    let preferences = load_hook_preferences(root, true)?;
    let cache_root = poly_cache::hook_sources_dir()?;
    std::fs::create_dir_all(&cache_root)
        .with_context(|| format!("creating hook source cache {}", cache_root.display()))?;
    let existing = read_lock(root)?;
    let mut locked = Vec::new();
    let mut resolved = Vec::new();
    for source in &hooks.sources {
        let (entry, source_root) = provision_source(root, &cache_root, source, existing.as_ref(), update)?;
        if let Some(entry) = entry {
            locked.push(entry);
        }
        let manifest = load_manifest(&source_root)?;
        resolved.extend(select_hooks(source, &source_root, manifest, &preferences, install)?);
    }
    if update {
        if locked.is_empty() {
            remove_lock(root)?;
        } else {
            write_lock(
                root,
                &HookSourceLock {
                    version: 1,
                    sources: locked,
                },
            )?;
        }
    }
    Ok(resolved)
}

fn provision_source(
    root: &Path,
    cache_root: &Path,
    source: &HookSource,
    existing: Option<&HookSourceLock>,
    update: bool,
) -> anyhow::Result<(Option<LockedSource>, PathBuf)> {
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
    let locked = existing.and_then(|lock| {
        lock.sources
            .iter()
            .find(|entry| entry.id == source.id && entry.source == url)
    });
    if !update {
        let locked = locked.ok_or_else(|| {
            anyhow::anyhow!(
                "Git hook source {:?} has no lock entry; run `poly hooks update` first",
                source.id
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
        return Ok((Some(locked.clone()), checkout));
    }
    let revision = source.revision.as_deref().expect("validated Git revision");
    let _guard = lock_source(&source_cache)?;
    ensure_mirror(&mirror, url)?;
    run_git(&mirror, &["fetch", "--quiet", "--force", "origin", revision])?;
    let resolved = git_output(&mirror, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    validate_locked_revision(&resolved)?;
    let checkout = source_cache.join("checkouts").join(&resolved);
    materialize_source_checkout(&mirror, &checkout, &resolved, &source.id)?;
    let cache_path = format!("cache://hook-sources/{source_key}/{resolved}");
    Ok((
        Some(LockedSource {
            id: source.id.clone(),
            source: url.to_string(),
            revision: resolved,
            path: cache_path,
        }),
        checkout,
    ))
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
    use crate::remote::run_command;

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
        let selected = provision(consumer.path(), &hooks, false, true).unwrap();
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
            provision(consumer.path(), &hooks, false, true)
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
        provision(consumer.path(), &hooks, false, false).unwrap();
        assert!(!producer.path().join("installed.txt").exists());
        provision(consumer.path(), &hooks, false, true).unwrap();
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
            provision(root.path(), &hooks, false, true)
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
                provision_source(Path::new("."), &cache_root, &source, None, true).unwrap()
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
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        make_writable(&checkout);
        std::fs::write(checkout.join(".git/HEAD"), "0000000000000000000000000000000000000000\n").unwrap();

        let (_, reconstructed) = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

        assert_eq!(reconstructed, checkout);
        assert_eq!(
            git_output(&checkout, &["rev-parse", "HEAD"]).unwrap(),
            lock.sources[0].revision
        );
    }

    #[test]
    fn normal_run_replaces_tampered_checkout() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        make_writable(&checkout);
        std::fs::write(
            checkout.join(PRODUCER_MANIFEST_NAME),
            "version=1\nhooks=[]\n# tampered\n",
        )
        .unwrap();

        provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

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
        let (locked, checkout) = provision_source(Path::new("."), cache.path(), &source, None, true).unwrap();
        let lock = HookSourceLock {
            version: 1,
            sources: vec![locked.unwrap()],
        };
        let source_cache = cache
            .path()
            .join(poly_cache::hook_source_key(source.git.as_deref().unwrap()));
        std::fs::remove_dir_all(source_cache.join("mirror.git")).unwrap();

        let (_, reused) = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap();

        assert_eq!(reused, checkout);
        assert!(!source_cache.join("mirror.git").exists());
        assert!(!checkout.join(".git/objects/info/alternates").exists());
    }

    #[test]
    fn normal_run_rejects_non_oid_lock_revision() {
        let producer = create_git_source();
        let cache = tempfile::tempdir().unwrap();
        let source = git_source(producer.path());
        let lock = HookSourceLock {
            version: 1,
            sources: vec![LockedSource {
                id: source.id.clone(),
                source: source.git.clone().unwrap(),
                revision: "../../outside".to_string(),
                path: "cache://invalid".to_string(),
            }],
        };

        let error = provision_source(Path::new("."), cache.path(), &source, Some(&lock), false).unwrap_err();

        assert!(error.to_string().contains("full hexadecimal Git object ID"));
    }
}
