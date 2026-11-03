//! Remote `extends` / hook-source fetching under offline mode.
//!
//! Alone in its binary (one `#[test]`) because it flips the process-wide offline
//! switch and the proxy environment. A loopback listener stands in for every
//! proxy, so any attempted connection is counted rather than timing out.

use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use poly_cli::remote::materialize;

fn git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn counting_proxy() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stream.is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    (format!("http://{address}"), hits)
}

#[test]
fn offline_uses_only_what_is_pinned_and_cached() {
    let (proxy, hits) = counting_proxy();
    // SAFETY: single test in this binary; nothing else touches the environment.
    unsafe {
        for name in [
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
        ] {
            std::env::set_var(name, &proxy);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let producer = tempfile::tempdir().unwrap();
    git(&["init", "--quiet"], producer.path());
    std::fs::write(producer.path().join("base.toml"), "x = 1\n").unwrap();
    git(&["add", "base.toml"], producer.path());
    git(
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
        producer.path(),
    );
    let oid = git(&["rev-parse", "HEAD"], producer.path());
    let url = producer.path().to_string_lossy().into_owned();
    let cache = tempfile::tempdir().unwrap();

    // Online: populate the cache from the local producer.
    let checkout = materialize(&url, &oid, cache.path(), false).unwrap();

    poly_config::network::set_offline(true);
    assert!(poly_cli::offline(), "the CLI view must delegate to the shared switch");

    // Offline: a pinned, cached revision still resolves.
    assert_eq!(materialize(&url, &oid, cache.path(), false).unwrap(), checkout);

    // Offline: a ref refresh and an uncached source both fail with the offline message.
    let refresh = materialize(&url, "HEAD", cache.path(), true).unwrap_err();
    assert!(format!("{refresh:#}").contains("offline mode"), "{refresh:#}");
    let uncached = materialize("https://example.invalid/base.git", &oid, cache.path(), false).unwrap_err();
    assert!(format!("{uncached:#}").contains("offline mode"), "{uncached:#}");

    assert_eq!(hits.load(Ordering::SeqCst), 0, "offline mode attempted a connection");
}
