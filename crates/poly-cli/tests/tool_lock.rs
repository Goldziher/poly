//! `poly lock` for downloadable tools: resolution against a local fake of the
//! GitHub releases API, the lock it writes, and what `--check` and `--upgrade`
//! make of it.
//!
//! Alone in its binary (one `#[test]`) because it points the process at the fake
//! through `POLY_GITHUB_API_URL` and `POLY_CACHE_HOME`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use poly_catalog::{Catalog, Source};
use poly_cli::config_sources::RemoteExtendsResolver;
use poly_cli::lock_cmd::{LockOutcome, sync};
use poly_cli::polylock;

type Handler = dyn Fn(&str) -> (u16, String) + Send + Sync;

struct Fake {
    base: String,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Fake {
    fn start(handler: Arc<Handler>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                let (status, body) = handler(&path);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Fake { base, hits }
    }

    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

fn source() -> &'static Source {
    Catalog::get()
        .tool("hadolint")
        .and_then(|tool| tool.recipe.as_ref())
        .and_then(|recipe| recipe.install.as_ref())
        .and_then(|install| install.source.as_ref())
        .expect("hadolint has a source")
}

/// A release carrying every platform's asset, each with the digest `digest`.
fn release(base: &str, tag: &str, digest: char) -> String {
    let version = tag.trim_start_matches('v');
    let assets: Vec<_> = source()
        .assets
        .values()
        .map(|spec| {
            let name = Source::asset_name(spec, version);
            serde_json::json!({
                "name": name,
                "browser_download_url": format!("{base}/dl/{tag}/{name}"),
                "digest": format!("sha256:{}", digest.to_string().repeat(64)),
            })
        })
        .collect();
    serde_json::json!({"tag_name": tag, "assets": assets}).to_string()
}

fn config(root: &Path, extra: &str) -> std::path::PathBuf {
    let path = root.join("poly.toml");
    std::fs::write(
        &path,
        format!("[tools.hadolint]\nenabled = true\ninstall = \"auto\"\n{extra}"),
    )
    .unwrap();
    path
}

fn locked(root: &Path) -> polylock::LockedTool {
    polylock::read(root).unwrap().expect("poly.lock").tools.remove(0)
}

#[test]
fn poly_lock_pins_tool_releases_and_check_and_upgrade_follow_them() {
    let cache = tempfile::tempdir().unwrap();
    let generation = Arc::new(AtomicUsize::new(0));
    let served = Arc::clone(&generation);
    let base_cell: Arc<Mutex<String>> = Arc::default();
    let base_for_handler = Arc::clone(&base_cell);
    let fake = Fake::start(Arc::new(move |path| {
        let base = base_for_handler.lock().unwrap().clone();
        match (path, served.load(Ordering::SeqCst)) {
            ("/repos/hadolint/hadolint/releases/latest", 0) => (200, release(&base, "v9.9.9", 'a')),
            ("/repos/hadolint/hadolint/releases/latest", 1) => (200, release(&base, "v9.9.10", 'b')),
            ("/repos/hadolint/hadolint/releases/tags/v1.0.0", _) => (200, release(&base, "v1.0.0", 'e')),
            _ => (404, "{}".to_owned()),
        }
    }));
    *base_cell.lock().unwrap() = fake.base.clone();
    // SAFETY: the only test in this binary; set before any thread reads the environment.
    unsafe {
        std::env::set_var("POLY_CACHE_HOME", cache.path());
        std::env::set_var("POLY_GITHUB_API_URL", &fake.base);
        std::env::remove_var("POLY_OFFLINE");
    }
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let config_path = config(root, "");

    // `latest` resolves into the lock, one entry per platform, with the API's digests.
    let LockOutcome::Written(lock) = sync(root, &config_path, false, false).unwrap() else {
        panic!("first lock must write");
    };
    let entry = lock.tool("hadolint").expect("hadolint locked");
    assert_eq!(entry.version, "9.9.9");
    assert_eq!(entry.assets.len(), source().assets.len());
    assert!(entry.assets.iter().all(|asset| asset.sha256 == "a".repeat(64)));
    assert_eq!(fake.count(), 1);

    // A run reads the lock: the provisioner is told what to download with no lookup.
    RemoteExtendsResolver::new(root).unwrap();
    assert_eq!(poly_config::tool_lock::get("hadolint").unwrap().version, "9.9.9");

    // The lock is current; neither re-locking nor checking asks upstream again.
    generation.store(1, Ordering::SeqCst);
    assert!(matches!(
        sync(root, &config_path, false, true).unwrap(),
        LockOutcome::InSync(_)
    ));
    assert!(matches!(
        sync(root, &config_path, false, false).unwrap(),
        LockOutcome::InSync(_)
    ));
    assert_eq!(fake.count(), 1, "a satisfied lock needs no network");

    // `--check --upgrade` sees the newer release as drift and writes nothing.
    assert_eq!(sync(root, &config_path, true, true).unwrap(), LockOutcome::Drift);
    assert_eq!(locked(root).version, "9.9.9");
    // `--upgrade` moves the pin.
    assert!(matches!(
        sync(root, &config_path, true, false).unwrap(),
        LockOutcome::Written(_)
    ));
    assert_eq!(locked(root).version, "9.9.10");

    // An exact version the catalog never listed locks through the source, and an edit
    // of `version` is drift for the offline check.
    let pinned = config(root, "version = \"1.0.0\"\n");
    assert_eq!(sync(root, &pinned, false, true).unwrap(), LockOutcome::Drift);
    assert!(matches!(
        sync(root, &pinned, false, false).unwrap(),
        LockOutcome::Written(_)
    ));
    assert_eq!(locked(root).version, "1.0.0");
    assert!(matches!(
        sync(root, &pinned, false, true).unwrap(),
        LockOutcome::InSync(_)
    ));

    // Disabling the tool drops its entry.
    std::fs::write(root.join("poly.toml"), "[tools.hadolint]\nenabled = false\n").unwrap();
    assert!(matches!(
        sync(root, &root.join("poly.toml"), false, false).unwrap(),
        LockOutcome::Written(lock) if lock.is_empty()
    ));
}
