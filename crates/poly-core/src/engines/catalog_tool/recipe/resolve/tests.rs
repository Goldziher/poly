//! Resolution against a scripted local server: no test touches the network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use poly_catalog::Source;

use super::cache::{self, DEFAULT_TTL, Release};
use super::{Network, Request, Resolver, Scope};
use crate::engines::catalog_tool::recipe::install::platform;
use crate::engines::catalog_tool::recipe::net::{Http, Policy, unix_now};
use crate::engines::catalog_tool::recipe::testserver::{Canned, Server};
use crate::engines::catalog_tool::recipe::unit::Retry;

const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LATEST: &str = "/repos/o/r/releases/latest";

pub(in crate::engines::catalog_tool::recipe) type Sleeps = Arc<Mutex<Vec<Duration>>>;

pub(in crate::engines::catalog_tool::recipe) fn fast_http(attempts: u32) -> (Http, Sleeps) {
    let sleeps: Sleeps = Arc::default();
    let record = Arc::clone(&sleeps);
    let policy = Policy {
        retry: Retry::new(attempts, 0, 0),
        connect: Duration::from_secs(2),
        stall: Duration::from_millis(300),
        request: Duration::from_millis(600),
        download: Duration::from_secs(5),
        max_retry_after: Duration::from_secs(30),
    };
    (
        Http::with(policy, Arc::new(move |wait| record.lock().unwrap().push(wait))),
        sleeps,
    )
}

fn resolver(server: &Server, root: &std::path::Path, offline: bool) -> (Resolver, Sleeps) {
    let (http, sleeps) = fast_http(3);
    (
        Resolver::with_http(http, &server.base, root.to_path_buf(), offline),
        sleeps,
    )
}

fn github_source() -> Source {
    serde_json::from_value(serde_json::json!({
        "kind": "binary",
        "github": "o/r",
        "assets": {platform(): {"name": "tool-{version}.tar.gz", "member": "tool-{version}/tool"}}
    }))
    .expect("source")
}

fn release_body(base: &str, tag: &str, digest: Option<&str>) -> String {
    let version = tag.trim_start_matches('v');
    let mut asset = serde_json::json!({
        "name": format!("tool-{version}.tar.gz"),
        "browser_download_url": format!("{base}/dl/{tag}/tool-{version}.tar.gz"),
    });
    if let Some(digest) = digest {
        asset["digest"] = format!("sha256:{digest}").into();
    }
    serde_json::json!({"tag_name": tag, "assets": [asset]}).to_string()
}

/// A server whose latest release is `v1.2.3`, with the given replies for it.
fn latest_server(replies: impl FnOnce(&str) -> Vec<Canned>) -> Server {
    Server::start_with(|base| vec![(LATEST.to_owned(), replies(base))])
}

fn ok_latest(base: &str) -> Canned {
    Canned::ok(release_body(base, "v1.2.3", Some(HEX_A))).header("ETag", "\"abc\"")
}

fn latest(resolver: &Resolver, root_ttl: Duration) -> Result<Arc<Release>, String> {
    resolver.resolve(
        "tool",
        &github_source(),
        &Request::Latest,
        root_ttl,
        Scope::Host,
        Network::Allowed,
    )
}

#[test]
fn latest_comes_from_the_api_with_its_digest() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let release = latest(&resolver, DEFAULT_TTL).expect("resolves");
    assert_eq!(release.version, "1.2.3");
    let asset = &release.assets[&platform()];
    assert_eq!(asset.sha256, HEX_A);
    assert_eq!(asset.url, format!("{}/dl/v1.2.3/tool-1.2.3.tar.gz", server.base));
    assert_eq!(asset.member.as_deref(), Some("tool-1.2.3/tool"));
}

#[test]
fn a_503_is_retried_and_the_next_answer_used() {
    let server = latest_server(|base| vec![Canned::new(503, "busy"), ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    assert_eq!(latest(&resolver, DEFAULT_TTL).expect("recovers").version, "1.2.3");
    assert_eq!(server.hits(LATEST), 2);
}

#[test]
fn a_429_waits_as_long_as_retry_after_asks() {
    let server = latest_server(|base| {
        vec![
            Canned::new(429, "slow down").header("Retry-After", "7"),
            ok_latest(base),
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, sleeps) = resolver(&server, dir.path(), false);
    latest(&resolver, DEFAULT_TTL).expect("recovers");
    assert_eq!(*sleeps.lock().unwrap(), [Duration::from_secs(7)]);
}

#[test]
fn a_retry_after_beyond_the_cap_fails_without_waiting() {
    let server = latest_server(|_| vec![Canned::new(503, "later").header("Retry-After", "3600")]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, sleeps) = resolver(&server, dir.path(), false);
    assert!(latest(&resolver, DEFAULT_TTL).is_err());
    assert!(sleeps.lock().unwrap().is_empty());
    assert_eq!(server.hits(LATEST), 1);
}

#[test]
fn a_request_that_stalls_times_out_instead_of_hanging() {
    let server = latest_server(|_| vec![Canned::ok("{}").delayed(Duration::from_secs(3))]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let started = std::time::Instant::now();
    let error = latest(&resolver, DEFAULT_TTL).expect_err("must time out");
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(server.hits(LATEST), 3, "every attempt timed out");
}

#[test]
fn a_client_error_other_than_429_is_not_retried() {
    let server = latest_server(|_| vec![Canned::new(401, "bad credentials")]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = latest(&resolver, DEFAULT_TTL).expect_err("401");
    assert!(error.contains("HTTP 401"), "{error}");
    assert_eq!(server.hits(LATEST), 1);
}

#[test]
fn an_exhausted_rate_limit_names_the_reset_and_the_token() {
    let reset = (unix_now() + 600).to_string();
    let server = latest_server(|_| {
        vec![
            Canned::new(403, "rate limited")
                .header("x-ratelimit-remaining", "0")
                .header("x-ratelimit-reset", &reset),
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = latest(&resolver, DEFAULT_TTL).expect_err("rate limited");
    for needle in ["rate limit", "GITHUB_TOKEN", "UTC", "`tool`"] {
        assert!(error.contains(needle), "{needle} missing: {error}");
    }
    assert_eq!(server.hits(LATEST), 1, "a quota that resets in minutes is not retried");
}

#[test]
fn a_cached_latest_within_the_ttl_asks_nobody() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    latest(&resolver(&server, dir.path(), false).0, DEFAULT_TTL).expect("first");
    let again = latest(&resolver(&server, dir.path(), false).0, DEFAULT_TTL).expect("second");
    assert_eq!(again.version, "1.2.3");
    assert_eq!(server.hits(LATEST), 1);
}

#[test]
fn one_run_resolves_a_tool_once() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    for _ in 0..5 {
        latest(&resolver, Duration::ZERO).expect("resolves");
    }
    assert_eq!(
        server.hits(LATEST),
        1,
        "ttl 0 would refetch each call without memoization"
    );
}

#[test]
fn an_expired_latest_is_revalidated_with_its_etag_and_a_304_keeps_it() {
    let server = latest_server(|base| vec![ok_latest(base), Canned::new(304, "")]);
    let dir = tempfile::tempdir().unwrap();
    latest(&resolver(&server, dir.path(), false).0, Duration::ZERO).expect("first");
    let revalidated = latest(&resolver(&server, dir.path(), false).0, Duration::ZERO).expect("second");
    assert_eq!(revalidated.version, "1.2.3");
    assert_eq!(revalidated.assets[&platform()].sha256, HEX_A);
    let requests = server.requests(LATEST);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].headers.get("if-none-match").map(String::as_str),
        Some("\"abc\"")
    );
    let stored = cache::read(&cache::latest_path(dir.path(), "tool")).expect("cached");
    assert!(stored.checked_at >= unix_now() - 5, "a 304 refreshes the TTL clock");
}

#[test]
fn a_failed_refresh_after_the_ttl_keeps_the_last_answer() {
    let server = latest_server(|_| vec![Canned::new(503, "down")]);
    let dir = tempfile::tempdir().unwrap();
    let stale = Release {
        version: "1.0.0".into(),
        assets: [(
            platform(),
            cache::ResolvedAsset {
                url: "http://example.invalid/old".into(),
                sha256: HEX_B.into(),
                member: None,
            },
        )]
        .into(),
        checked_at: 1,
        ..Release::default()
    };
    cache::write(&cache::latest_path(dir.path(), "tool"), &stale).unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let used = latest(&resolver, DEFAULT_TTL).expect("falls back to the cache");
    assert_eq!(used.version, "1.0.0");
    assert_eq!(server.hits(LATEST), 3, "the refresh was attempted, with retries");
}

#[test]
fn offline_uses_the_cache_without_any_request() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    latest(&resolver(&server, dir.path(), false).0, DEFAULT_TTL).expect("online first");
    let (offline, _) = resolver(&server, dir.path(), true);
    assert_eq!(latest(&offline, Duration::ZERO).expect("cached").version, "1.2.3");
    assert_eq!(server.hits(LATEST), 1);
}

#[test]
fn offline_with_nothing_cached_says_so_by_tool() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let (offline, _) = resolver(&server, dir.path(), true);
    let error = latest(&offline, DEFAULT_TTL).expect_err("nothing to use");
    assert!(error.contains("offline mode") && error.contains("`tool`"), "{error}");
    assert_eq!(server.hits(LATEST), 0, "offline must not touch the network");
}

#[test]
fn an_older_version_the_catalog_never_listed_resolves_by_tag() {
    let server = Server::start_with(|base| {
        vec![(
            "/repos/o/r/releases/tags/v0.9.0".to_owned(),
            vec![Canned::ok(release_body(base, "v0.9.0", Some(HEX_B)))],
        )]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let source = github_source();
    let exact = Request::Exact("0.9.0".into());
    let release = resolver
        .resolve("tool", &source, &exact, DEFAULT_TTL, Scope::Host, Network::Allowed)
        .expect("exact");
    assert_eq!(release.version, "0.9.0");
    assert_eq!(release.assets[&platform()].sha256, HEX_B);
    let (offline, _) = self::resolver(&server, dir.path(), true);
    offline
        .resolve("tool", &source, &exact, DEFAULT_TTL, Scope::Host, Network::Allowed)
        .expect("an exact version, once resolved, needs no network");
    assert_eq!(server.hits("/repos/o/r/releases/tags/v0.9.0"), 1);
}

#[test]
fn an_unknown_version_lists_the_closest_published_ones() {
    let server = Server::start_with(|base| {
        let tags: Vec<_> = ["v2.15.0", "v2.14.1", "v1.0.0"]
            .iter()
            .map(|tag| serde_json::json!({"tag_name": tag}))
            .collect();
        let _ = base;
        vec![
            (
                "/repos/o/r/releases/tags/v2.14.2".to_owned(),
                vec![Canned::new(404, "{}")],
            ),
            (
                "/repos/o/r/releases?per_page=100".to_owned(),
                vec![Canned::ok(serde_json::Value::Array(tags).to_string())],
            ),
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = resolver
        .resolve(
            "tool",
            &github_source(),
            &Request::Exact("2.14.2".into()),
            DEFAULT_TTL,
            Scope::Host,
            Network::Allowed,
        )
        .expect_err("unknown");
    assert!(error.contains("no release 2.14.2"), "{error}");
    assert!(error.contains("closest published: [2.14.1, 2.15.0"), "{error}");
}

#[test]
fn a_missing_asset_names_the_platforms_poly_knows() {
    let server = latest_server(|base| {
        let body = release_body(base, "v1.2.3", Some(HEX_A)).replace("tool-1.2.3.tar.gz", "other-1.2.3.tar.gz");
        vec![Canned::ok(body)]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = latest(&resolver, DEFAULT_TTL).expect_err("asset renamed upstream");
    assert!(
        error.contains("no asset `tool-1.2.3.tar.gz`") && error.contains(&platform()),
        "{error}"
    );
    assert!(error.contains("set `bin`"), "{error}");
}

#[test]
fn a_platform_the_source_lacks_is_reported_with_the_ones_it_has() {
    let source: Source = serde_json::from_value(serde_json::json!({
        "kind": "binary", "github": "o/r",
        "assets": {"plan9-mips": {"name": "tool-{version}"}}
    }))
    .unwrap();
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = resolver
        .resolve(
            "tool",
            &source,
            &Request::Latest,
            DEFAULT_TTL,
            Scope::Host,
            Network::Allowed,
        )
        .expect_err("no asset for this host");
    assert!(error.contains("upstream platforms: [plan9-mips]"), "{error}");
}

#[test]
fn an_asset_without_an_api_digest_uses_the_checksum_list() {
    let list = format!("{HEX_B}  tool-1.2.3.tar.gz\n");
    let server = Server::start_with(|base| {
        vec![
            (LATEST.to_owned(), vec![Canned::ok(release_body(base, "v1.2.3", None))]),
            ("/dl/v1.2.3/checksums.txt".to_owned(), vec![Canned::ok(list)]),
        ]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let mut source = github_source();
    source.checksum = Some(poly_catalog::Checksum {
        sidecar: None,
        list: Some("checksums.txt".into()),
    });
    let release = resolver
        .resolve(
            "tool",
            &source,
            &Request::Latest,
            DEFAULT_TTL,
            Scope::Host,
            Network::Allowed,
        )
        .expect("resolves through the list");
    assert_eq!(release.assets[&platform()].sha256, HEX_B);
    assert_eq!(server.hits("/dl/v1.2.3/checksums.txt"), 1);
}

#[test]
fn a_url_source_takes_its_version_from_maven_metadata_and_its_digest_from_a_sidecar() {
    let server = Server::start_with(|_| {
        vec![
            (
                "/meta/maven-metadata.xml".to_owned(),
                vec![Canned::ok(
                    "<metadata><versioning><release>3.1.0</release></versioning></metadata>",
                )],
            ),
            ("/dl/3.1.0/tool-3.1.0.bin.sha256".to_owned(), vec![Canned::ok(HEX_A)]),
        ]
    });
    let source: Source = serde_json::from_value(serde_json::json!({
        "kind": "binary",
        "base_url": format!("{}/dl/{{version}}", server.base),
        "latest": {"maven_metadata": server.url("/meta/maven-metadata.xml")},
        "checksum": {"sidecar": "{url}.sha256"},
        "assets": {"any": {"name": "tool-{version}.bin"}}
    }))
    .unwrap();
    source.validate().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let release = resolver
        .resolve(
            "tool",
            &source,
            &Request::Latest,
            DEFAULT_TTL,
            Scope::Host,
            Network::Allowed,
        )
        .expect("resolves");
    assert_eq!(release.version, "3.1.0");
    let asset = &release.assets["any"];
    assert_eq!(asset.url, format!("{}/dl/3.1.0/tool-3.1.0.bin", server.base));
    assert_eq!(asset.sha256, HEX_A);
}

#[test]
fn a_url_source_without_the_sidecar_says_the_version_is_not_published() {
    let server = Server::start_with(|_| {
        vec![(
            "/meta/maven-metadata.xml".to_owned(),
            vec![Canned::ok(
                "<metadata><versioning><release>3.1.0</release><versions><version>3.0.0</version><version>3.1.0</version></versions></versioning></metadata>",
            )],
        )]
    });
    let source: Source = serde_json::from_value(serde_json::json!({
        "kind": "binary",
        "base_url": format!("{}/dl/{{version}}", server.base),
        "latest": {"maven_metadata": server.url("/meta/maven-metadata.xml")},
        "checksum": {"sidecar": "{url}.sha256"},
        "assets": {"any": {"name": "tool-{version}.bin"}}
    }))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = resolver
        .resolve(
            "tool",
            &source,
            &Request::Exact("3.0.1".into()),
            DEFAULT_TTL,
            Scope::Host,
            Network::Allowed,
        )
        .expect_err("unknown");
    assert!(error.contains("no release 3.0.1") && error.contains("3.0.0"), "{error}");
}

#[test]
fn two_processes_resolving_at_once_ask_upstream_once() {
    let server = latest_server(|base| vec![ok_latest(base).delayed(Duration::from_millis(150))]);
    let dir = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let (resolver, _) = resolver(&server, dir.path(), false);
                assert_eq!(latest(&resolver, DEFAULT_TTL).expect("resolves").version, "1.2.3");
            });
        }
    });
    assert_eq!(
        server.hits(LATEST),
        1,
        "the file lock serializes, the cache answers the rest"
    );
}

#[test]
fn a_request_parses_latest_and_versions() {
    assert_eq!(Request::of(None), Ok(Request::Latest));
    assert_eq!(Request::of(Some("latest")), Ok(Request::Latest));
    assert_eq!(Request::of(Some("v2.14.1")), Ok(Request::Exact("2.14.1".into())));
    assert_eq!(Request::of(Some("2.14.1")), Ok(Request::Exact("2.14.1".into())));
}

#[test]
fn a_version_that_could_escape_a_path_or_url_is_refused() {
    for bad in ["../../etc", "1.0/../x", "1..0", "a b", "1.0?x=y", "1.0#frag"] {
        let error = Request::of(Some(bad)).expect_err(bad);
        assert!(error.contains("not a version"), "{bad}: {error}");
    }
}

#[test]
fn an_upstream_tag_that_is_not_a_safe_version_is_refused() {
    let server = latest_server(|base| vec![Canned::ok(release_body(base, "v1.0/../../evil", Some(HEX_A)))]);
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let error = latest(&resolver, DEFAULT_TTL).expect_err("hostile tag");
    assert!(error.contains("does not fit the tag template"), "{error}");
    assert!(!dir.path().join("..").join("evil").exists());
}

/// A source publishing the host platform and one more, each with its own asset.
fn two_platform_source() -> Source {
    serde_json::from_value(serde_json::json!({
        "kind": "binary",
        "github": "o/r",
        "assets": {
            platform(): {"name": "tool-{version}.tar.gz"},
            "plan9-mips": {"name": "tool-{version}.mips.tar.gz"},
        }
    }))
    .expect("source")
}

fn two_platform_body(base: &str) -> String {
    let asset = |name: &str| {
        serde_json::json!({
            "name": name,
            "browser_download_url": format!("{base}/dl/{name}"),
            "digest": format!("sha256:{HEX_A}"),
        })
    };
    serde_json::json!({"tag_name": "v1.2.3", "assets": [
        asset("tool-1.2.3.tar.gz"), asset("tool-1.2.3.mips.tar.gz")]})
    .to_string()
}

fn resolve_all(resolver: &Resolver, source: &Source) -> Result<Arc<Release>, String> {
    resolver.resolve(
        "tool",
        source,
        &Request::Latest,
        Duration::ZERO,
        Scope::All,
        Network::Allowed,
    )
}

#[test]
fn a_host_only_cache_is_not_revalidated_into_a_one_platform_lock() {
    let source = two_platform_source();
    let server = latest_server(|base| vec![Canned::ok(two_platform_body(base))]);
    let dir = tempfile::tempdir().unwrap();
    let host_only = Release {
        version: "1.2.3".into(),
        assets: [(
            platform(),
            cache::ResolvedAsset {
                url: "http://example.invalid/host".into(),
                sha256: HEX_A.into(),
                member: None,
            },
        )]
        .into(),
        checked_at: 1,
        etag: Some("\"abc\"".into()),
        last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".into()),
    };
    cache::write(&cache::latest_path(dir.path(), "tool"), &host_only).unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let release = resolve_all(&resolver, &source).expect("all platforms");
    assert_eq!(
        release.assets.len(),
        2,
        "{:?}",
        release.assets.keys().collect::<Vec<_>>()
    );
    let sent = &server.requests(LATEST)[0];
    assert!(
        !sent.headers.contains_key("if-none-match") && !sent.headers.contains_key("if-modified-since"),
        "a cache that cannot answer must not ask for a 304: {:?}",
        sent.headers
    );
}

#[test]
fn a_304_to_a_request_the_cache_cannot_answer_is_an_error_not_a_partial_lock() {
    let source = two_platform_source();
    let server = latest_server(|_| vec![Canned::new(304, "")]);
    let dir = tempfile::tempdir().unwrap();
    let host_only = Release {
        version: "1.2.3".into(),
        assets: [(
            platform(),
            cache::ResolvedAsset {
                url: "http://example.invalid/host".into(),
                sha256: HEX_A.into(),
                member: None,
            },
        )]
        .into(),
        checked_at: 1,
        etag: Some("\"abc\"".into()),
        ..Release::default()
    };
    cache::write(&cache::latest_path(dir.path(), "tool"), &host_only).unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    assert!(resolve_all(&resolver, &source).is_err());
}

#[test]
fn a_platform_missing_upstream_is_left_out_while_the_others_lock() {
    let source = two_platform_source();
    let server = latest_server(|base| {
        let body = two_platform_body(base).replace("tool-1.2.3.mips.tar.gz", "other.tar.gz");
        vec![Canned::ok(body)]
    });
    let dir = tempfile::tempdir().unwrap();
    let (resolver, _) = resolver(&server, dir.path(), false);
    let release = resolve_all(&resolver, &source).expect("the host platform still resolves");
    assert_eq!(release.assets.keys().collect::<Vec<_>>(), [&platform()]);
}

#[test]
fn offline_resolution_neither_locks_nor_creates_the_cache() {
    let server = latest_server(|base| vec![ok_latest(base)]);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("never-created");
    let (offline, _) = resolver(&server, &root, true);
    latest(&offline, DEFAULT_TTL).expect_err("nothing cached");
    let denied = Resolver::with_http(fast_http(1).0, &server.base, root.clone(), false);
    denied
        .resolve(
            "tool",
            &github_source(),
            &Request::Latest,
            DEFAULT_TTL,
            Scope::Host,
            Network::Denied,
        )
        .expect_err("nothing cached");
    assert!(
        !root.exists(),
        "reading the cache must not create directories or take its lock"
    );
}

#[test]
fn the_token_goes_only_to_the_apis_exact_https_origin() {
    let (http, _) = fast_http(1);
    let mut resolver = Resolver::with_http(http, "https://api.github.com", std::env::temp_dir(), false);
    resolver.token = Some("secret".into());
    let auth = |url: &str| resolver.headers(url).iter().any(|(name, _)| name == "Authorization");
    assert!(auth("https://api.github.com/repos/o/r/releases/latest"));
    assert!(auth("https://API.github.com:443/repos/o/r"));
    assert!(!auth("https://api.github.com.evil.example/repos/o/r"));
    assert!(!auth("https://api.github.com:8443/repos/o/r"));
    assert!(!auth("http://api.github.com/repos/o/r"));
    assert!(!auth("https://user@api.github.com/repos/o/r"));
    assert!(!auth("https://github.com/o/r/releases/download/x"));

    let (http, _) = fast_http(1);
    let mut plain = Resolver::with_http(http, "http://127.0.0.1:123", std::env::temp_dir(), false);
    plain.token = Some("secret".into());
    assert!(
        plain
            .headers("http://127.0.0.1:123/x")
            .iter()
            .all(|(name, _)| name != "Authorization"),
        "never over http"
    );
    assert!(
        plain.headers("http://127.0.0.1:1234/x").is_empty(),
        "another port is another origin"
    );
}
