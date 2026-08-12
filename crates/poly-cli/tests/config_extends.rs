//! Integration coverage for the `extends` remote-config resolver + lock flow.
//!
//! Each test builds a real temporary git repository as a config base and drives
//! the `poly-cli` resolver (`RemoteExtendsResolver`) and `config_sources::update`
//! end to end. `POLY_CACHE_HOME` is redirected at a process-shared tempdir so the
//! real per-user cache is never touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Once, OnceLock};

use poly_cli::config_sources::{self, RemoteExtendsResolver};
use poly_config::{Patterns, PolyConfig};

/// Redirect `POLY_CACHE_HOME` at a process-shared tempdir exactly once. Remote
/// sources are keyed by URL and every test uses a unique temp repo, so one shared
/// cache home is safe; the `Once` guard serializes the single env write.
fn init_cache_home() {
    static INIT: Once = Once::new();
    static CACHE_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    INIT.call_once(|| {
        let dir = tempfile::tempdir().expect("create cache home");
        // SAFETY: run exactly once (via `Once`) before any other test thread
        // reads `POLY_CACHE_HOME`, so there is no concurrent env access.
        unsafe {
            std::env::set_var("POLY_CACHE_HOME", dir.path());
        }
        let _ = CACHE_DIR.set(dir);
    });
}

fn git(args: &[&str], cwd: &Path) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// Create a git repository whose root `poly.toml` holds `config`, returning the
/// repo dir, its URL (its path), and the committed object ID.
fn create_git_base(config: &str) -> (tempfile::TempDir, String, String) {
    let repo = tempfile::tempdir().expect("temp repo");
    git(&["init", "--quiet"], repo.path());
    std::fs::write(repo.path().join("poly.toml"), config).expect("write base config");
    git(&["add", "poly.toml"], repo.path());
    git(
        &[
            "-c",
            "user.name=Poly Test",
            "-c",
            "user.email=poly@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            "base config",
        ],
        repo.path(),
    );
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8 oid")
    .trim()
    .to_string();
    // Forward slashes: valid as a local git URL on Windows and TOML-safe when
    // interpolated into an `extends` basic string (backslashes are illegal TOML escapes).
    let url = repo.path().to_string_lossy().replace('\\', "/");
    (repo, url, oid)
}

/// A consumer directory (not itself a git repo) containing a `poly.toml`.
fn consumer(config: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("consumer dir");
    let path = dir.path().join("poly.toml");
    std::fs::write(&path, config).expect("write consumer config");
    (dir, path)
}

#[test]
fn full_oid_git_base_merges_and_child_overrides() {
    init_cache_home();
    let (_base, url, oid) = create_git_base("[defaults]\nline_length = 77\ntrim_trailing_whitespace = false\n");
    let (_consumer, config_path) = consumer(&format!(
        "extends = [{{ git = \"{url}\", revision = \"{oid}\", file = \"poly.toml\" }}]\n[defaults]\nline_length = 99\n"
    ));

    let resolver = RemoteExtendsResolver::new(config_path.parent().unwrap()).unwrap();
    let config = PolyConfig::load_file_with(&config_path, &resolver).expect("load with full-oid base");

    // Child override wins; the base-only value survives the merge.
    assert_eq!(config.defaults.line_length, 99);
    assert!(!config.defaults.trim_trailing_whitespace);
}

/// The point of a shared baseline: a repo adds one exclude of its own and still
/// receives the base's list — including later changes to it — instead of holding
/// a frozen copy. `exclude_mode = "replace"` is the opt-out.
#[test]
fn remote_base_exclude_globs_accumulate_under_the_consumers_own() {
    init_cache_home();
    let (_base, url, oid) =
        create_git_base("[discovery]\nexclude = [\"vendor/**\", \"target/**\"]\n[hooks.builtin]\nlint = true\n");
    let source = format!("{{ git = \"{url}\", revision = \"{oid}\", file = \"poly.toml\" }}");

    let (_consumer, config_path) = consumer(&format!(
        "extends = [{source}]\n[discovery]\nexclude = [\"generated/**\"]\n"
    ));
    let resolver = RemoteExtendsResolver::new(config_path.parent().unwrap()).unwrap();
    let config = PolyConfig::load_file_with(&config_path, &resolver).expect("load with remote base");
    assert_eq!(
        config.discovery.exclude.as_slice(),
        &[
            "vendor/**".to_string(),
            "target/**".to_string(),
            "generated/**".to_string()
        ],
    );
    assert_eq!(
        config.hooks.builtin.lint.exclude.as_ref().map(Patterns::as_slice),
        Some(
            &[
                "vendor/**".to_string(),
                "target/**".to_string(),
                "generated/**".to_string()
            ][..]
        ),
        "the base's hooks see the merged exclude list too"
    );

    let (_replacing, replacing_path) = consumer(&format!(
        "extends = [{source}]\n[discovery]\nexclude = [\"generated/**\"]\nexclude_mode = \"replace\"\n"
    ));
    let resolver = RemoteExtendsResolver::new(replacing_path.parent().unwrap()).unwrap();
    let replaced = PolyConfig::load_file_with(&replacing_path, &resolver).expect("load with remote base");
    assert_eq!(replaced.discovery.exclude.as_slice(), &["generated/**".to_string()]);
}

#[test]
fn update_locks_symbolic_ref_then_offline_load_succeeds() {
    init_cache_home();
    let (_base, url, oid) = create_git_base("[defaults]\nline_length = 55\n");
    let (consumer_dir, config_path) = consumer(&format!(
        "extends = [{{ git = \"{url}\", revision = \"HEAD\", file = \"poly.toml\" }}]\n[defaults]\nfinal_newline = false\n"
    ));
    let root = consumer_dir.path();

    let lock = config_sources::update(root, &config_path).expect("update writes lock");
    assert_eq!(lock.source_count(), 1);

    let lock_path = root.join("poly-config.lock");
    let lock_text = std::fs::read_to_string(&lock_path).expect("read lock");
    assert!(
        lock_text.contains(&format!("locked = \"{oid}\"")),
        "lock records resolved oid:\n{lock_text}"
    );
    assert!(
        lock_text.contains("revision = \"HEAD\""),
        "lock records declared ref:\n{lock_text}"
    );

    // A fresh resolver reads the lock; the load resolves the pinned oid offline.
    let resolver = RemoteExtendsResolver::new(root).unwrap();
    let config = PolyConfig::load_file_with(&config_path, &resolver).expect("offline load via lock");
    assert_eq!(config.defaults.line_length, 55); // from the base ~keep
    assert!(!config.defaults.final_newline); // child override ~keep
}

#[test]
fn symbolic_ref_without_lock_entry_errors() {
    init_cache_home();
    let (_base, url, _oid) = create_git_base("[defaults]\nline_length = 40\n");
    let (consumer_dir, config_path) = consumer(&format!(
        "extends = [{{ git = \"{url}\", revision = \"HEAD\", file = \"poly.toml\" }}]\n"
    ));

    let resolver = RemoteExtendsResolver::new(consumer_dir.path()).unwrap();
    let error = PolyConfig::load_file_with(&config_path, &resolver).unwrap_err();
    let chain = format!("{error:#}");
    assert!(chain.contains("run `poly config update` first"), "{chain}");
}

#[test]
fn local_override_wins_over_remote_base() {
    init_cache_home();
    let (_base, url, oid) = create_git_base("[defaults]\nline_length = 55\n");
    let (consumer_dir, config_path) = consumer(&format!(
        "extends = [{{ git = \"{url}\", revision = \"{oid}\", file = \"poly.toml\" }}]\n"
    ));
    // poly.local.toml is the final layer, above the remote base and this poly.toml.
    std::fs::write(
        consumer_dir.path().join("poly.local.toml"),
        "[defaults]\nline_length = 123\n",
    )
    .unwrap();

    let resolver = RemoteExtendsResolver::new(consumer_dir.path()).unwrap();
    let config = PolyConfig::load_file_with(&config_path, &resolver).expect("load");
    assert_eq!(
        config.defaults.line_length, 123,
        "poly.local.toml must win over a remote base"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_base_file_escaping_checkout_is_rejected() {
    init_cache_home();
    // A secret outside any checkout that a malicious base tries to exfiltrate.
    let outside = tempfile::tempdir().expect("outside dir");
    let secret = outside.path().join("secret.toml");
    std::fs::write(&secret, "[defaults]\nline_length = 1\n").unwrap();

    // A base repo whose `poly.toml` is a symlink pointing at the outside secret.
    let repo = tempfile::tempdir().expect("temp repo");
    git(&["init", "--quiet"], repo.path());
    std::os::unix::fs::symlink(&secret, repo.path().join("poly.toml")).expect("symlink base config");
    git(&["add", "poly.toml"], repo.path());
    git(
        &[
            "-c",
            "user.name=Poly Test",
            "-c",
            "user.email=poly@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            "malicious symlink",
        ],
        repo.path(),
    );
    let oid = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    let url = repo.path().to_string_lossy().replace('\\', "/");

    let (consumer_dir, config_path) = consumer(&format!(
        "extends = [{{ git = \"{url}\", revision = \"{oid}\", file = \"poly.toml\" }}]\n"
    ));
    let resolver = RemoteExtendsResolver::new(consumer_dir.path()).unwrap();
    let error = PolyConfig::load_file_with(&config_path, &resolver).unwrap_err();
    let chain = format!("{error:#}");
    assert!(
        chain.contains("outside its checkout"),
        "symlink escape must be rejected: {chain}"
    );
}

#[test]
fn path_base_resolves_like_local_resolver() {
    init_cache_home();
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("base.toml"),
        "[defaults]\nline_length = 66\ntrim_trailing_whitespace = false\n",
    )
    .expect("write base");
    let config_path = workspace.path().join("poly.toml");
    std::fs::write(
        &config_path,
        "extends = [{ path = \"./base.toml\" }]\n[defaults]\nline_length = 88\n",
    )
    .expect("write consumer");

    let resolver = RemoteExtendsResolver::new(workspace.path()).unwrap();
    let config = PolyConfig::load_file_with(&config_path, &resolver).expect("load path base");
    assert_eq!(config.defaults.line_length, 88);
    assert!(!config.defaults.trim_trailing_whitespace);
}
