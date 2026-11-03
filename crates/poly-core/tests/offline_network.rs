//! Offline mode must make zero network attempts when a grammar is not cached.
//!
//! Env mutation and the process-wide offline switch live in this binary alone
//! (one `#[test]`) so no other test observes them. HTTP(S) proxies point at a
//! local listener that records connections, so a download attempt is visible
//! deterministically instead of as a timeout.
#![cfg(feature = "all-engines")]

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Accept connections on a loopback port, counting each one.
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
fn offline_never_downloads_an_uncached_grammar() {
    let (proxy, hits) = counting_proxy();
    // SAFETY: this binary has a single test and no other thread reads the
    // environment while it is mutated.
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

    poly_config::network::set_offline(true);
    let started = std::time::Instant::now();
    assert!(!poly_core::engines::grammar_available("zz-uncached-offline-grammar"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "offline lookup must not wait on a socket"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0, "offline mode attempted a connection");
}
