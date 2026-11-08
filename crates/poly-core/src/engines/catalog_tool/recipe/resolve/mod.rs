//! Resolving which release of a tool to download, and the digest it must have.
//!
//! A `Source` (poly-catalog) says where releases live; this module asks. `latest`
//! comes from the GitHub releases API or a Maven `maven-metadata.xml`, an exact
//! version from the same host, and each asset's SHA-256 from the API's `digest`
//! field or the publisher's checksum file. Answers are cached under poly's cache
//! (see [`cache`]) so a run asks upstream at most once per TTL, and a conditional
//! request (`If-None-Match`) makes even that ask free of API quota when nothing
//! changed.
//!
//! A refresh that fails after the TTL keeps the last answer and warns once; it
//! never blocks a run. Offline, only the cache answers. One [`Resolver`] memoizes
//! per run, so many units of one tool resolve once.

pub(in crate::engines::catalog_tool) mod cache;
mod digest;
mod github;

pub(in crate::engines::catalog_tool) use self::digest::{valid_version, version_key};

#[cfg(test)]
pub(in crate::engines::catalog_tool) mod tests;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use poly_catalog::Source;

pub(in crate::engines::catalog_tool) use self::cache::{FileLock, Release, ResolvedAsset};
use self::github::ApiRelease;
use super::net::{FetchError, Http, Reply, unix_now, utc};

const GITHUB_API: &str = "https://api.github.com";

/// Environment variable that redirects the GitHub API (GitHub Enterprise, mirrors).
pub(in crate::engines::catalog_tool) const API_ENV: &str = "POLY_GITHUB_API_URL";

/// Which release is wanted.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::engines::catalog_tool) enum Request {
    /// The newest upstream release.
    Latest,
    /// This exact version.
    Exact(String),
}

impl Request {
    /// `latest`/absent is [`Request::Latest`]; anything else an exact version
    /// (a leading `v` is dropped: tags and versions are written both ways).
    ///
    /// # Errors
    ///
    /// A version with anything but `[A-Za-z0-9._+-]`, or a `..`, is refused: it
    /// ends up in URLs and cache paths.
    pub(in crate::engines::catalog_tool) fn of(version: Option<&str>) -> Result<Self, String> {
        match version.map(str::trim) {
            None | Some("" | "latest") => Ok(Request::Latest),
            Some(exact) => {
                let exact = exact.strip_prefix('v').unwrap_or(exact);
                if digest::valid_version(exact) {
                    Ok(Request::Exact(exact.to_owned()))
                } else {
                    Err(format!(
                        "`{exact}` is not a version: use letters, digits and `.`, `_`, `+`, `-` only (no `..`)"
                    ))
                }
            }
        }
    }
}

/// Which platforms' assets are needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::engines::catalog_tool) enum Scope {
    /// The running host only (a run).
    Host,
    /// Every platform the source names (`poly lock`).
    All,
}

/// Whether resolution may ask the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::engines::catalog_tool) enum Network {
    /// Ask upstream when the cache cannot answer.
    Allowed,
    /// Cache only: the config did not allow downloads.
    Denied,
}

/// Why a release could not be resolved: the message, and whether the cause is
/// the network or upstream being unavailable (so a cached answer may stand in).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fail {
    message: String,
    unavailable: bool,
}

impl Fail {
    fn new(message: String) -> Self {
        Fail {
            message,
            unavailable: false,
        }
    }
}

type Memo = HashMap<String, Result<Arc<Release>, String>>;

/// Resolves releases for a run.
pub(in crate::engines::catalog_tool) struct Resolver {
    http: Http,
    api: String,
    token: Option<String>,
    root: Option<PathBuf>,
    offline: Option<bool>,
    memo: Mutex<Memo>,
}

impl Resolver {
    /// The process-wide resolver: poly's cache, `GITHUB_TOKEN`, the real API.
    pub(in crate::engines::catalog_tool) fn shared() -> &'static Resolver {
        static SHARED: OnceLock<Resolver> = OnceLock::new();
        SHARED.get_or_init(|| Resolver {
            http: Http::production(),
            api: std::env::var(API_ENV).unwrap_or_else(|_| GITHUB_API.to_owned()),
            token: ["GITHUB_TOKEN", "GH_TOKEN"]
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty())),
            root: None,
            offline: None,
            memo: Mutex::new(HashMap::new()),
        })
    }

    /// A resolver against a local fixture: its own cache root, no token, a
    /// zero-wait retry policy whose sleeps are discarded.
    #[cfg(test)]
    pub(in crate::engines::catalog_tool) fn for_test(api: &str, root: PathBuf, offline: bool) -> Self {
        let (http, _) = tests::fast_http(3);
        Resolver::with_http(http, api, root, offline)
    }

    #[cfg(test)]
    fn with_http(http: Http, api: &str, root: PathBuf, offline: bool) -> Self {
        Resolver {
            http,
            api: api.trim_end_matches('/').to_owned(),
            token: None,
            root: Some(root),
            offline: Some(offline),
            memo: Mutex::new(HashMap::new()),
        }
    }

    fn offline(&self) -> bool {
        self.offline.unwrap_or_else(poly_config::network::offline)
    }

    /// Why the network may not be used, if it may not.
    fn no_network(&self, network: Network) -> Option<&'static str> {
        if self.offline() {
            Some("poly is offline (offline mode)")
        } else if network == Network::Denied {
            Some("`install` is not \"auto\", so poly does not look anything up")
        } else {
            None
        }
    }

    /// The directory holding every tool's state: `<cache>/tools`.
    pub(in crate::engines::catalog_tool) fn root(&self) -> Result<PathBuf, String> {
        match &self.root {
            Some(root) => Ok(root.clone()),
            None => poly_cache::cache_home()
                .map(|home| home.join("tools"))
                .map_err(|error| format!("locating poly's tool cache failed: {error:#}")),
        }
    }

    /// Resolve `request` for `tool`. `ttl` bounds how stale a cached `latest` may
    /// be before it is revalidated.
    ///
    /// # Errors
    ///
    /// The message says what was attempted, against which URL, and what to do.
    pub(in crate::engines::catalog_tool) fn resolve(
        &self,
        tool: &str,
        source: &Source,
        request: &Request,
        ttl: Duration,
        scope: Scope,
        network: Network,
    ) -> Result<Arc<Release>, String> {
        let key = format!("{tool}\0{request:?}\0{scope:?}\0{network:?}\0{ttl:?}\0{}", self.api);
        if let Some(done) = self
            .memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return done.clone();
        }
        let outcome = self
            .resolve_uncached(tool, source, request, ttl, scope, network)
            .map(Arc::new);
        self.memo
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, outcome.clone());
        outcome
    }

    fn resolve_uncached(
        &self,
        tool: &str,
        source: &Source,
        request: &Request,
        ttl: Duration,
        scope: Scope,
        network: Network,
    ) -> Result<Release, String> {
        let keys = wanted_keys(tool, source, scope)?;
        let root = self.root()?;
        // Only a run that may ask upstream writes the cache, so only it needs the
        // lock (and the directory the lock lives in); reading never does.
        let _held = if self.no_network(network).is_none() {
            Some(
                FileLock::acquire(&cache::tool_dir(&root, tool).join(".resolve.lock"))
                    .map_err(|error| format!("tool `{tool}`: {error:#}"))?,
            )
        } else {
            None
        };
        match request {
            Request::Latest => self.latest(tool, source, &root, ttl, &keys, network),
            Request::Exact(version) => self.exact(tool, source, &root, version, &keys, network),
        }
    }

    fn latest(
        &self,
        tool: &str,
        source: &Source,
        root: &std::path::Path,
        ttl: Duration,
        keys: &[String],
        network: Network,
    ) -> Result<Release, String> {
        let path = cache::latest_path(root, tool);
        let cached = cache::read(&path);
        if let Some(why) = self.no_network(network) {
            return match cached {
                Some(release) if release.covers(keys) => Ok(release),
                Some(release) => Err(unresolved_here(tool, &release.version, why)),
                None => Err(unresolved_nothing(tool, why)),
            };
        }
        if let Some(release) = &cached
            && release.covers(keys)
            && unix_now().saturating_sub(release.checked_at) < ttl.as_secs()
        {
            return Ok(release.clone());
        }
        match self.fetch_latest(tool, source, cached.as_ref(), keys) {
            Ok(release) => {
                let _ = cache::write(&path, &release);
                Ok(release)
            }
            Err(fail) => match cached {
                Some(stale) if fail.unavailable && stale.covers(keys) => {
                    cache::warn_once(
                        &format!("stale:{tool}"),
                        &format!(
                            "could not refresh the latest release of `{tool}` ({}); using {}",
                            fail.message, stale.version
                        ),
                    );
                    Ok(stale)
                }
                _ => Err(fail.message),
            },
        }
    }

    fn exact(
        &self,
        tool: &str,
        source: &Source,
        root: &std::path::Path,
        version: &str,
        keys: &[String],
        network: Network,
    ) -> Result<Release, String> {
        let path = cache::version_path(root, tool, version);
        let cached = cache::read(&path);
        if let Some(release) = &cached
            && release.covers(keys)
        {
            return Ok(release.clone());
        }
        if let Some(why) = self.no_network(network) {
            return Err(format!(
                "tool `{tool}`: version {version} has not been resolved for this platform and {why}; \
                 run `poly lock` online, or provide `url` and `sha256`"
            ));
        }
        let mut release = match source.github.as_deref() {
            Some(repo) => self.github_exact(tool, source, repo, version, keys)?,
            None => Release {
                version: version.to_owned(),
                assets: cached.map(|c| c.assets).unwrap_or_default(),
                ..Release::default()
            },
        };
        release.checked_at = unix_now();
        self.fill_digests(tool, source, &mut release, keys)
            .map_err(|fail| self.unknown_or(tool, source, version, fail))?;
        let _ = cache::write(&path, &release);
        Ok(release)
    }

    fn fetch_latest(
        &self,
        tool: &str,
        source: &Source,
        cached: Option<&Release>,
        keys: &[String],
    ) -> Result<Release, Fail> {
        let (url, github) = self.latest_url(source);
        let mut headers = self.headers(&url);
        // Validators only for an answer that would serve this request: a 304
        // keeps the cached record, which must then cover every wanted platform.
        let cached = cached.filter(|cached| cached.covers(keys));
        if let Some(cached) = cached {
            if let Some(etag) = &cached.etag {
                headers.push(("If-None-Match".into(), etag.clone()));
            }
            if let Some(modified) = &cached.last_modified {
                headers.push(("If-Modified-Since".into(), modified.clone()));
            }
        }
        let borrowed: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        let doing = "looking up the latest release";
        let reply = self
            .http
            .get_text(&url, &borrowed)
            .map_err(|error| self.explain(tool, doing, &error))?;
        if reply.not_modified()
            && let Some(cached) = cached
        {
            let mut kept = cached.clone();
            kept.checked_at = unix_now();
            return Ok(kept);
        }
        let mut release = if github {
            let api: ApiRelease = github::parse(&reply.body, &url).map_err(Fail::new)?;
            let latest_template = source.latest_tag_template();
            let version = poly_catalog::version_from_tag(latest_template, &api.tag_name)
                .filter(|version| digest::valid_version(version))
                .ok_or_else(|| {
                    Fail::new(format!(
                        "tool `{tool}`: the latest release tag `{}` does not fit the tag template `{latest_template}`",
                        api.tag_name
                    ))
                })?;
            if source.github.is_some() {
                self.github_assets(tool, source, &api, &version, keys)?
            } else {
                Release {
                    version,
                    ..Release::default()
                }
            }
        } else {
            let version = digest::maven_release(&reply.body)
                .ok_or_else(|| Fail::new(format!("tool `{tool}`: {url} lists no release version")))?;
            Release {
                version,
                ..Release::default()
            }
        };
        if let Some(cached) = cached
            && cached.version == release.version
        {
            for (key, asset) in &cached.assets {
                release.assets.entry(key.clone()).or_insert_with(|| asset.clone());
            }
        }
        remember(&mut release, &reply);
        self.fill_digests(tool, source, &mut release, keys)?;
        Ok(release)
    }

    /// The URL that names the newest version, and whether it is a GitHub API one.
    fn latest_url(&self, source: &Source) -> (String, bool) {
        if let Some(repo) = &source.github {
            return (format!("{}/repos/{repo}/releases/latest", self.api), true);
        }
        let latest = source.latest.as_ref();
        match (
            latest.and_then(|l| l.github.as_deref()),
            latest.and_then(|l| l.maven_metadata.as_deref()),
        ) {
            (Some(repo), _) => (format!("{}/repos/{repo}/releases/latest", self.api), true),
            (None, Some(url)) => (url.to_owned(), false),
            (None, None) => (String::new(), false),
        }
    }

    fn github_exact(
        &self,
        tool: &str,
        source: &Source,
        repo: &str,
        version: &str,
        keys: &[String],
    ) -> Result<Release, String> {
        let tag = source.tag_of(version);
        let url = format!("{}/repos/{repo}/releases/tags/{tag}", self.api);
        let headers = self.headers(&url);
        let borrowed: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        let doing = format!("looking up release {tag}");
        match self.http.get_text(&url, &borrowed) {
            Ok(reply) => {
                let api: ApiRelease = github::parse(&reply.body, &url)?;
                self.github_assets(tool, source, &api, version, keys)
                    .map_err(|fail| fail.message)
            }
            Err(error) if error.status() == Some(404) => Err(self.unknown_version(tool, source, version)),
            Err(error) => Err(self.explain(tool, &doing, &error).message),
        }
    }

    /// Build a release from a GitHub API answer, taking each asset's digest from
    /// the API or, failing that, the source's checksum file.
    fn github_assets(
        &self,
        tool: &str,
        source: &Source,
        api: &ApiRelease,
        version: &str,
        keys: &[String],
    ) -> Result<Release, Fail> {
        // One wanted asset must exist; of several (a lock covers every platform),
        // upstream dropping one must not stop the others from locking.
        let strict = keys.len() == 1;
        let mut release = Release {
            version: version.to_owned(),
            ..Release::default()
        };
        for key in keys {
            let spec = &source.assets[key];
            let name = Source::asset_name(spec, version);
            let Some(found) = api.assets.iter().find(|asset| asset.name == name) else {
                if strict {
                    return Err(Fail::new(missing_asset(tool, source, api, &name, key, version)));
                }
                cache::warn_once(
                    &format!("missing-asset:{tool}:{version}:{key}"),
                    &format!(
                        "tool `{tool}` {version} publishes no `{name}` for {key}; \
                         it is left out of the lock and that platform cannot download it"
                    ),
                );
                continue;
            };
            let sha256 = found.digest.as_deref().and_then(digest::from_api_digest);
            release.assets.insert(
                key.clone(),
                ResolvedAsset {
                    url: found.browser_download_url.clone(),
                    sha256: sha256.unwrap_or_default(),
                    member: Source::member(spec, version),
                },
            );
        }
        Ok(release)
    }

    /// Fill in the digest of every wanted asset that has none: a URL source's
    /// assets, and a GitHub asset whose API entry carried no `digest`.
    fn fill_digests(&self, tool: &str, source: &Source, release: &mut Release, keys: &[String]) -> Result<(), Fail> {
        let version = release.version.clone();
        let mut list: Option<String> = None;
        for key in keys {
            if release.assets.get(key).is_some_and(|asset| !asset.sha256.is_empty()) {
                continue;
            }
            let spec = &source.assets[key];
            let name = Source::asset_name(spec, &version);
            let url = match release.assets.get(key) {
                Some(asset) => asset.url.clone(),
                None if source.github.is_some() => continue,
                None => source.asset_url(&version, &name).unwrap_or_default(),
            };
            let Some(checksum) = &source.checksum else {
                return Err(Fail::new(format!(
                    "tool `{tool}`: release {version} publishes no digest for `{name}` and the catalog knows no checksum file; \
                     give `url` and `sha256` to pin it"
                )));
            };
            let sha256 = if let Some(sidecar) = checksum.sidecar_url(&url) {
                let reply = self.plain(&sidecar, tool, "fetching a checksum")?;
                digest::from_sidecar(&reply.body, &name)
                    .ok_or_else(|| Fail::new(format!("tool `{tool}`: {sidecar} does not hold a SHA-256 digest")))?
            } else {
                if list.is_none() {
                    let listed = checksum.list_name(&version).unwrap_or_default();
                    let base = url.rsplit_once('/').map_or_else(
                        || source.download_base(&version).unwrap_or_default(),
                        |(directory, _)| directory.to_owned(),
                    );
                    list = Some(
                        self.plain(&format!("{base}/{listed}"), tool, "fetching the checksum list")?
                            .body,
                    );
                }
                digest::from_list(list.as_deref().unwrap_or_default(), &name)
                    .ok_or_else(|| Fail::new(format!("tool `{tool}`: the checksum list has no entry for `{name}`")))?
            };
            release.assets.insert(
                key.clone(),
                ResolvedAsset {
                    url,
                    sha256,
                    member: Source::member(spec, &version),
                },
            );
        }
        Ok(())
    }

    fn plain(&self, url: &str, tool: &str, doing: &str) -> Result<Reply, Fail> {
        let headers = self.headers(url);
        let borrowed: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        self.http.get_text(url, &borrowed).map_err(|error| {
            let mut fail = self.explain(tool, doing, &error);
            if error.status() == Some(404) {
                fail.message = format!("tool `{tool}`: {url} does not exist, so that version is not published there");
            }
            fail
        })
    }

    /// Request headers for `url`. The token goes only to the GitHub API's exact
    /// origin (scheme, host and port), and only over https.
    fn headers(&self, url: &str) -> Vec<(String, String)> {
        let mut headers = Vec::new();
        if let (Some(target), Some(api)) = (Origin::of(url), Origin::of(&self.api))
            && target == api
        {
            headers.push(("Accept".to_owned(), "application/vnd.github+json".to_owned()));
            if let Some(token) = &self.token
                && target.scheme == "https"
            {
                headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
            }
        }
        headers
    }

    fn explain(&self, tool: &str, doing: &str, error: &FetchError) -> Fail {
        let mut message = match error {
            FetchError::Status {
                rate_limit: Some(limit),
                url,
                ..
            } => {
                let reset = limit
                    .reset
                    .map(|at| {
                        format!(
                            " The quota resets at {} (in {} min).",
                            utc(at),
                            at.saturating_sub(unix_now()).div_ceil(60)
                        )
                    })
                    .unwrap_or_default();
                let advice = if self.token.is_some() {
                    "The GITHUB_TOKEN in use is exhausted too"
                } else {
                    "Set GITHUB_TOKEN to raise the limit from 60 to 5000 requests an hour"
                };
                format!(
                    "tool `{tool}`: GitHub's API rate limit is exhausted while {doing} ({url}).{reset} {advice}, \
                     or pin `version` and run `poly lock` so runs need no lookup."
                )
            }
            other => format!("tool `{tool}`: {other} while {doing}."),
        };
        if error.is_unavailable() {
            message.push_str(" Nothing usable is cached; retry later, or set `bin` to a host install.");
        }
        Fail {
            message,
            unavailable: error.is_unavailable(),
        }
    }

    /// A failure while checking an exact version: a 404 means upstream has no such version.
    fn unknown_or(&self, tool: &str, source: &Source, version: &str, fail: Fail) -> String {
        if fail.message.contains("does not exist") {
            return self.unknown_version(tool, source, version);
        }
        fail.message
    }

    fn unknown_version(&self, tool: &str, source: &Source, version: &str) -> String {
        let published = self.published_versions(source);
        let near = digest::closest(version, &published, 5);
        let hint = if near.is_empty() {
            String::new()
        } else {
            format!("; closest published: [{}]", near.join(", "))
        };
        format!("tool `{tool}`: upstream has no release {version}{hint}")
    }

    /// Versions upstream lists, best effort (an error is an empty list).
    fn published_versions(&self, source: &Source) -> Vec<String> {
        let latest = source.latest.as_ref();
        let repo = source
            .github
            .as_deref()
            .or_else(|| latest.and_then(|l| l.github.as_deref()));
        if let Some(repo) = repo {
            let url = format!("{}/repos/{repo}/releases?per_page=100", self.api);
            let headers = self.headers(&url);
            let borrowed: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
            return self
                .http
                .get_text(&url, &borrowed)
                .ok()
                .and_then(|reply| serde_json::from_str::<Vec<ApiRelease>>(&reply.body).ok())
                .map(|releases| {
                    releases
                        .iter()
                        .filter_map(|r| poly_catalog::version_from_tag(source.latest_tag_template(), &r.tag_name))
                        .filter(|version| digest::valid_version(version))
                        .collect()
                })
                .unwrap_or_default();
        }
        if let Some(url) = latest.and_then(|l| l.maven_metadata.as_deref()) {
            return self
                .http
                .get_text(url, &[])
                .ok()
                .map(|reply| digest::maven_versions(&reply.body))
                .unwrap_or_default();
        }
        Vec::new()
    }
}

/// The scheme, host and port of a URL, compared whole so that
/// `https://api.github.com.evil.example` or another port is never the API.
#[derive(Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    fn of(url: &str) -> Option<Origin> {
        let (scheme, rest) = url.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        let authority = rest.split(['/', '?', '#']).next()?;
        if authority.contains('@') {
            return None;
        }
        let default = match scheme.as_str() {
            "https" => 443,
            "http" => 80,
            _ => return None,
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || host.starts_with('[') => (host, port.parse().ok()?),
            _ => (authority, default),
        };
        (!host.is_empty()).then(|| Origin {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

fn remember(release: &mut Release, reply: &Reply) {
    release.checked_at = unix_now();
    release.etag = reply.etag.clone();
    release.last_modified = reply.last_modified.clone();
}

fn unresolved_nothing(tool: &str, why: &str) -> String {
    format!(
        "tool `{tool}`: no release has been resolved yet and {why}; \
         run `poly lock` online, or set `install = \"auto\"` and a `version`"
    )
}

fn unresolved_here(tool: &str, version: &str, why: &str) -> String {
    format!(
        "tool `{tool}`: the cached release {version} was never resolved for this platform and {why}; \
         run `poly lock` online"
    )
}

/// The `Source::assets` keys a scope needs.
fn wanted_keys(tool: &str, source: &Source, scope: Scope) -> Result<Vec<String>, String> {
    match scope {
        Scope::All => Ok(source.assets.keys().cloned().collect()),
        Scope::Host => {
            let platform = super::install::platform();
            let key = if source.assets.contains_key(&platform) {
                platform.clone()
            } else if source.assets.contains_key("any") {
                "any".to_owned()
            } else {
                let published: Vec<&str> = source.assets.keys().map(String::as_str).collect();
                return Err(format!(
                    "tool `{tool}` publishes no download for {platform}; upstream platforms: [{}]. \
                     Install it yourself and set `bin`",
                    published.join(", ")
                ));
            };
            Ok(vec![key])
        }
    }
}

fn missing_asset(tool: &str, source: &Source, api: &ApiRelease, name: &str, key: &str, version: &str) -> String {
    let published: BTreeMap<&str, ()> = api.assets.iter().map(|asset| (asset.name.as_str(), ())).collect();
    let platforms: Vec<&str> = source.assets.keys().map(String::as_str).collect();
    let near: Vec<String> = digest::closest(name, &published.keys().map(|n| (*n).to_owned()).collect::<Vec<_>>(), 3);
    format!(
        "tool `{tool}`: release {version} has no asset `{name}` for {key}; poly knows platforms [{}], upstream \
         lists e.g. [{}]. Install it yourself and set `bin`",
        platforms.join(", "),
        near.join(", ")
    )
}
