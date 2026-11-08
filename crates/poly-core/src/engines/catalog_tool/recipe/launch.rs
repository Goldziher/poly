//! Finding the program that runs a recipe tool.
//!
//! The order, first match wins:
//!
//! 1. `[tools.<name>] bin` — an explicit executable (for a JVM tool, the `java`,
//!    which still needs the jar pin of step 2). A binary `bin` is checked before
//!    any pin is resolved: it never costs a lookup, and a broken pin cannot hide it.
//! 2. The pinned release in poly's tool cache, when `version` names one (or
//!    `install = "auto"` implies `latest`) — a pin beats whatever is on `PATH`.
//! 3. The pinned release downloaded now, when `install = "auto"`.
//! 4. The tool's binary on `PATH`, only when no `version` is pinned: a pin that
//!    cannot be honoured makes the tool unavailable instead of running another
//!    version. The one exception is an implicit `latest` (`install = "auto"`, no
//!    `version`) that cannot be looked up: that falls back to the newest seeded
//!    release, else the `PATH` binary, with a warning, because the user pinned
//!    nothing that a fallback could violate.
//!
//! A tool that is nowhere is [`Resolved::Missing`] with a message that says what
//! to run, never a silent no-op: a linter that did not run must not look like a
//! linter that found nothing.

use std::path::{Path, PathBuf};

use poly_catalog::{Asset, Recipe, Release, ReleaseKind, Tool};
use poly_config::{InstallPolicy, ToolConfig};

use super::install;
use super::resolve::cache;
use super::resolve::{self, Network, Request, Resolver, Scope};

/// How to start the tool: a program and the arguments that precede its own.
#[derive(Debug, Clone)]
pub(in crate::engines::catalog_tool) struct Launcher {
    pub(in crate::engines::catalog_tool) program: PathBuf,
    /// `-jar <jar>` for a JVM tool, empty otherwise.
    pub(in crate::engines::catalog_tool) prefix: Vec<String>,
    /// What the cache key folds in so a different tool build never reuses
    /// another's results.
    pub(in crate::engines::catalog_tool) identity: String,
}

pub(in crate::engines::catalog_tool) enum Resolved {
    Ready(Launcher),
    Missing(String),
}

pub(in crate::engines::catalog_tool) fn resolve(tool: &Tool, recipe: &Recipe, config: &ToolConfig) -> Resolved {
    if !recipe.java
        && let Some(bin) = &config.bin
    {
        return match locate(bin) {
            Some(path) => ready(path, Vec::new(), "bin"),
            None => Resolved::Missing(format!("`bin = \"{bin}\"` for tool `{}` does not exist", tool.name)),
        };
    }
    let pinned = match pinned_release(Resolver::shared(), tool, recipe, config) {
        Ok(pinned) => pinned,
        Err(message) => return Resolved::Missing(message),
    };
    if recipe.java {
        return resolve_jvm(tool, recipe, config, pinned);
    }
    if let Some(pin) = &pinned {
        return match provision(tool, config, pin) {
            Ok(path) => ready(path, Vec::new(), &pin.version),
            Err(reason) => Resolved::Missing(unmet_pin(tool, pin, &reason)),
        };
    }
    match which::which(&tool.binary) {
        Ok(path) => ready(path, Vec::new(), "path"),
        Err(_) => Resolved::Missing(missing_message(tool, recipe, pinned.as_ref())),
    }
}

fn resolve_jvm(tool: &Tool, recipe: &Recipe, config: &ToolConfig, pinned: Option<Pinned>) -> Resolved {
    if let Some(pin) = &pinned {
        let jar = match provision(tool, config, pin) {
            Ok(jar) => jar,
            Err(reason) => return Resolved::Missing(unmet_pin(tool, pin, &reason)),
        };
        let Some(java) = java_program(config) else {
            return Resolved::Missing(format!(
                "tool `{}` is a JVM tool but no `java` was found; set `bin` to one or put it on PATH",
                tool.name
            ));
        };
        let prefix = vec!["-jar".to_owned(), jar.to_string_lossy().into_owned()];
        return ready(java, prefix, &pin.version);
    }
    match which::which(&tool.binary) {
        Ok(path) => ready(path, Vec::new(), "path"),
        Err(_) => Resolved::Missing(missing_message(tool, recipe, pinned.as_ref())),
    }
}

/// The `java` to launch a jar with: `bin`, else `JAVA_HOME` from the tool's
/// `env` or the process's, else `PATH`.
fn java_program(config: &ToolConfig) -> Option<PathBuf> {
    if let Some(bin) = &config.bin {
        return locate(bin);
    }
    let home = config
        .env
        .get("JAVA_HOME")
        .cloned()
        .or_else(|| std::env::var("JAVA_HOME").ok());
    if let Some(home) = home {
        let candidate = Path::new(&home).join("bin/java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which::which("java").ok()
}

fn locate(bin: &str) -> Option<PathBuf> {
    let path = Path::new(bin);
    if path.components().count() > 1 {
        path.is_file().then(|| path.to_path_buf())
    } else {
        which::which(bin).ok()
    }
}

fn ready(program: PathBuf, prefix: Vec<String>, origin: &str) -> Resolved {
    let identity = identity(&program, &prefix, origin);
    Resolved::Ready(Launcher {
        program,
        prefix,
        identity,
    })
}

/// A string that changes when the executable (or the jar it launches) does:
/// its resolved path, size and modification time. `--version` would be more
/// readable but not every tool answers it, and spawning a JVM to ask costs
/// 400 ms on every run.
fn identity(program: &Path, prefix: &[String], origin: &str) -> String {
    let target = match prefix {
        [_, jar] => Path::new(jar),
        _ => program,
    };
    let stamp = std::fs::metadata(target).ok().map(|meta| {
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        format!("{}:{modified}", meta.len())
    });
    format!(
        "{origin}:{}:{}",
        std::fs::canonicalize(target)
            .unwrap_or_else(|_| target.to_path_buf())
            .display(),
        stamp.unwrap_or_default()
    )
}

/// A release the config pins, with the asset that serves this host.
#[derive(Debug)]
struct Pinned {
    version: String,
    asset: Asset,
    kind: ReleaseKind,
}

/// The release this tool is configured to run, or `None` when the config pins
/// nothing and the tool is taken from `PATH`.
///
/// Where the answer comes from, first match wins: the config's own `url` and
/// `sha256`; the repository's `poly.lock`; the catalog's seeded releases (for an
/// exact version); the tool's release source (the resolution cache, then
/// upstream). With no `version`, a tool poly can download and `install = "auto"`
/// mean `latest`.
fn pinned_release(
    resolver: &Resolver,
    tool: &Tool,
    recipe: &Recipe,
    config: &ToolConfig,
) -> Result<Option<Pinned>, String> {
    let install = recipe.install.as_ref();
    let source = install.and_then(|install| install.source.as_ref());
    let kind = if recipe.java {
        ReleaseKind::Jar
    } else {
        ReleaseKind::Binary
    };
    if let (Some(version), Some(url), Some(sha256)) = (&config.version, &config.url, &config.sha256) {
        if !resolve::valid_version(version) {
            return Err(format!("tool `{}`: `{version}` is not a version", tool.name));
        }
        return Ok(Some(Pinned {
            version: version.clone(),
            asset: Asset {
                url: url.clone(),
                sha256: sha256.to_ascii_lowercase(),
                member: None,
            },
            kind,
        }));
    }
    let request = match (&config.version, config.install, source) {
        (Some(version), _, _) => {
            Request::of(Some(version)).map_err(|message| format!("tool `{}`: {message}", tool.name))?
        }
        (None, InstallPolicy::Auto, Some(_)) => Request::Latest,
        (None, _, _) => return Ok(None),
    };
    if let Some(pinned) = locked_pin(tool, &request, source.map_or(kind, |source| source.kind))? {
        return Ok(Some(pinned));
    }
    let seeds = install.map(|install| &install.releases);
    let mut seed_gap = None;
    if let (Request::Exact(version), Some(seeds)) = (&request, seeds)
        && let Some(release) = seeds.get(version)
    {
        match seeded(tool, version, release) {
            Ok(pinned) => return Ok(Some(pinned)),
            Err(message) => seed_gap = Some(message),
        }
    }
    let Some(source) = source else {
        return Err(seed_gap.unwrap_or_else(|| no_source(tool, &request, seeds)));
    };
    let network = if poly_config::network::offline() || config.install != InstallPolicy::Auto {
        Network::Denied
    } else {
        Network::Allowed
    };
    let ttl = match config.resolve_ttl.as_deref() {
        Some(text) => cache::parse_ttl(text).map_err(|error| format!("tool `{}`: {error:#}", tool.name))?,
        None => cache::DEFAULT_TTL,
    };
    match resolver.resolve(&tool.name, source, &request, ttl, Scope::Host, network) {
        Ok(release) => {
            let key = release
                .assets
                .keys()
                .find(|key| **key == install::platform())
                .or_else(|| release.assets.keys().next());
            let asset = key.and_then(|key| release.assets.get(key)).ok_or_else(|| {
                format!(
                    "tool `{}` {} has no download for {}",
                    tool.name,
                    release.version,
                    install::platform()
                )
            })?;
            Ok(Some(Pinned {
                version: release.version.clone(),
                asset: asset.to_asset(),
                kind: source.kind,
            }))
        }
        Err(message) => {
            let newest_seed = seeds.and_then(|seeds| {
                seeds
                    .iter()
                    .filter(|(_, release)| install::asset_for(release).is_some())
                    .max_by_key(|(version, _)| resolve::version_key(version))
            });
            match (&request, network, newest_seed) {
                (Request::Latest, Network::Denied, Some((version, release))) => {
                    seeded(tool, version, release).map(Some)
                }
                (Request::Latest, _, seed) if config.version.is_none() => Ok(implicit_fallback(tool, &message, seed)),
                _ => Err(seed_gap.map_or(message.clone(), |gap| format!("{gap} ({message})"))),
            }
        }
    }
}

/// What an implicit `latest` that could not be resolved runs instead: the newest
/// seeded release, else (`None`) the tool on `PATH`. Says so once.
fn implicit_fallback(tool: &Tool, message: &str, seed: Option<(&String, &Release)>) -> Option<Pinned> {
    let fallback = seed.and_then(|(version, release)| seeded(tool, version, release).ok());
    let using = fallback.as_ref().map_or_else(
        || format!("the `{}` on PATH, if any", tool.binary),
        |pin| format!("the bundled release {}", pin.version),
    );
    cache::warn_once(
        &format!("implicit-latest:{}", tool.name),
        &format!(
            "could not look up the latest `{}` ({message}); using {using}. Pin `version` or run `poly lock` to avoid this",
            tool.name
        ),
    );
    fallback
}

/// The release `poly.lock` pins for this tool, when it answers `request`: any
/// lock serves `latest`, and an exact version needs that very version.
fn locked_pin(tool: &Tool, request: &Request, kind: ReleaseKind) -> Result<Option<Pinned>, String> {
    let Some(locked) = poly_config::tool_lock::get(&tool.name) else {
        return Ok(None);
    };
    let answers = match request {
        Request::Latest => true,
        Request::Exact(version) => *version == locked.version,
    };
    if !answers {
        return Ok(None);
    }
    let Some(asset) = locked.asset(&install::platform()) else {
        return Err(format!(
            "poly.lock pins tool `{}` {} but has no download for {}; run `poly lock` on a supported platform, \
             or install it yourself and set `bin`",
            tool.name,
            locked.version,
            install::platform()
        ));
    };
    Ok(Some(Pinned {
        version: locked.version.clone(),
        asset: Asset {
            url: asset.url.clone(),
            sha256: asset.sha256.clone(),
            member: asset.member.clone(),
        },
        kind,
    }))
}

fn seeded(tool: &Tool, version: &str, release: &Release) -> Result<Pinned, String> {
    let Some(asset) = install::asset_for(release) else {
        return Err(format!(
            "tool `{}` {version} has no download for {}; install it yourself and set `bin`",
            tool.name,
            install::platform()
        ));
    };
    Ok(Pinned {
        version: version.to_owned(),
        asset: asset.clone(),
        kind: release.kind,
    })
}

fn no_source(tool: &Tool, request: &Request, seeds: Option<&std::collections::BTreeMap<String, Release>>) -> String {
    let known: Vec<&str> = seeds
        .into_iter()
        .flat_map(|seeds| seeds.keys().map(String::as_str))
        .collect();
    let wanted = match request {
        Request::Latest => "latest".to_owned(),
        Request::Exact(version) => version.clone(),
    };
    format!(
        "tool `{}` has no release source to resolve {wanted} from; known: [{}]. Give `url` and `sha256` to pin another.",
        tool.name,
        known.join(", ")
    )
}

/// The installed path of the pinned release, downloading it first when the
/// config allows.
fn provision(tool: &Tool, config: &ToolConfig, pin: &Pinned) -> Result<PathBuf, String> {
    let path = install::destination(&tool.name, &pin.version, &pin.asset)
        .map_err(|error| format!("locating poly's tool cache failed: {error:#}"))?;
    if install::is_installed(&path, &pin.asset) {
        return Ok(path);
    }
    if config.install != InstallPolicy::Auto {
        return Err(format!(
            "release {} is not in poly's tool cache and `install` is not \"auto\"",
            pin.version
        ));
    }
    install::ensure(&path, &pin.asset, pin.kind)
        .map(|()| path)
        .map_err(|error| format!("installing release {} failed: {error:#}", pin.version))
}

/// The message for a `version` pin poly cannot honour. A pin that falls back to
/// whatever `PATH` holds would lint with a tool the config did not ask for.
fn unmet_pin(tool: &Tool, pin: &Pinned, reason: &str) -> String {
    format!(
        "tool `{name}` is pinned to version {version} in [tools.{name}] but {reason}; \
         poly will not run a different `{binary}` from PATH instead. Set `install = \"auto\"`, \
         point `bin` at the pinned build, or remove `version`",
        name = tool.name,
        version = pin.version,
        binary = tool.binary,
    )
}

fn missing_message(tool: &Tool, recipe: &Recipe, pinned: Option<&Pinned>) -> String {
    let hint = recipe.install.as_ref().map_or("", |install| install.hint.as_str());
    let mut message = format!(
        "tool `{}` is enabled in [tools.{}] but `{}` was not found on PATH",
        tool.name, tool.name, tool.binary
    );
    if !hint.is_empty() {
        message.push_str(&format!("; install it with: {hint}"));
    }
    if let Some(pin) = pinned {
        message.push_str(&format!(
            "; or set `install = \"auto\"` and poly will download the pinned {} release into its cache",
            pin.version
        ));
    }
    message
}

#[cfg(test)]
mod tests {
    use poly_catalog::{Catalog, Source};
    use poly_config::tool_lock::{self, LockedAsset, LockedTool};

    use super::*;
    use crate::engines::catalog_tool::recipe::testserver::{Canned, Server};

    /// Serializes tests that read or write the process-global lock, and empties it
    /// again on drop so a failing test cannot leak its pins into another.
    struct LockGuard {
        _held: std::sync::MutexGuard<'static, ()>,
    }

    impl LockGuard {
        fn new() -> Self {
            static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let held = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            tool_lock::install(Vec::new());
            LockGuard { _held: held }
        }
    }

    impl Drop for LockGuard {
        fn drop(&mut self) {
            tool_lock::install(Vec::new());
        }
    }

    const HEX: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn setup(name: &str) -> (&'static Tool, &'static Recipe, &'static Source) {
        let tool = Catalog::get().tool(name).expect("tool");
        let recipe = tool.recipe.as_ref().expect("recipe");
        let source = recipe.install.as_ref().and_then(|i| i.source.as_ref()).expect("source");
        (tool, recipe, source)
    }

    fn auto(version: Option<&str>) -> ToolConfig {
        ToolConfig {
            enabled: true,
            install: InstallPolicy::Auto,
            version: version.map(str::to_owned),
            ..ToolConfig::default()
        }
    }

    /// The release body GitHub would serve for `tag` of `source`, naming the host asset.
    fn release_for(base: &str, source: &Source, tag: &str) -> String {
        let version = tag.trim_start_matches('v');
        let spec = source.asset_spec(&install::platform()).expect("host asset");
        let name = Source::asset_name(spec, version);
        serde_json::json!({"tag_name": tag, "assets": [{
            "name": name,
            "browser_download_url": format!("{base}/dl/{tag}/{name}"),
            "digest": format!("sha256:{HEX}"),
        }]})
        .to_string()
    }

    #[test]
    fn a_locked_tool_is_pinned_with_no_resolution_traffic() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("tflint");
        tool_lock::install(vec![LockedTool {
            name: "tflint".into(),
            version: "0.1.0".into(),
            assets: vec![LockedAsset {
                platform: install::platform(),
                url: "https://example.invalid/tflint.zip".into(),
                sha256: HEX.into(),
                member: Some("tflint".into()),
            }],
        }]);
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        for version in [None, Some("latest"), Some("0.1.0")] {
            let pinned = pinned_release(&resolver, tool, recipe, &auto(version))
                .expect("locked")
                .expect("pinned");
            assert_eq!(pinned.version, "0.1.0");
            assert_eq!(pinned.asset.sha256, HEX);
            assert_eq!(pinned.asset.url, "https://example.invalid/tflint.zip");
        }
        assert_eq!(server.total(), 0, "a lock entry means no network at all");
        assert!(!dir.path().join("tflint").exists(), "nor any cache traffic");
    }

    #[test]
    fn a_version_the_catalog_never_listed_is_resolved_through_the_source() {
        let _lock = LockGuard::new();
        let (tool, recipe, source) = setup("hadolint");
        let server = Server::start_with(|base| {
            vec![(
                "/repos/hadolint/hadolint/releases/tags/v0.0.7".to_owned(),
                vec![Canned::ok(release_for(base, source, "v0.0.7"))],
            )]
        });
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let pinned = pinned_release(&resolver, tool, recipe, &auto(Some("0.0.7")))
            .unwrap()
            .unwrap();
        assert_eq!(pinned.version, "0.0.7");
        assert_eq!(pinned.asset.sha256, HEX);
        assert!(pinned.asset.url.starts_with(&server.base), "{}", pinned.asset.url);
    }

    #[test]
    fn a_seeded_version_needs_no_lookup() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("buf");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let seeded = recipe.install.as_ref().unwrap().releases.keys().next().unwrap().clone();
        let pinned = pinned_release(&resolver, tool, recipe, &auto(Some(&seeded)))
            .unwrap()
            .unwrap();
        assert_eq!(pinned.version, seeded);
        assert_eq!(server.total(), 0, "old pins keep working without asking upstream");
    }

    /// `recipe` with the host platform removed from every seeded release.
    fn without_host_seeds(recipe: &Recipe) -> Recipe {
        let mut recipe = recipe.clone();
        for release in recipe.install.as_mut().unwrap().releases.values_mut() {
            release.assets.remove(&install::platform());
        }
        recipe
    }

    #[test]
    fn a_seed_lacking_the_host_platform_is_resolved_through_the_source() {
        let _lock = LockGuard::new();
        let (tool, recipe, source) = setup("actionlint");
        let recipe = without_host_seeds(recipe);
        let seeded = recipe.install.as_ref().unwrap().releases.keys().next().unwrap().clone();
        let tag = format!("v{seeded}");
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_with(|base| {
            vec![(
                format!("/repos/rhysd/actionlint/releases/tags/{tag}"),
                vec![Canned::ok(release_for(base, source, &tag))],
            )]
        });
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let pinned = pinned_release(&resolver, tool, &recipe, &auto(Some(&seeded)))
            .unwrap()
            .unwrap();
        assert_eq!(pinned.version, seeded);
        assert!(server.total() > 0, "the version was resolved upstream");
    }

    #[test]
    fn a_seed_lacking_the_host_platform_offline_gives_the_actionable_error() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let recipe = without_host_seeds(recipe);
        let seeded = recipe.install.as_ref().unwrap().releases.keys().next().unwrap().clone();
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let error = pinned_release(&resolver, tool, &recipe, &auto(Some(&seeded))).expect_err("unresolvable");
        assert!(
            error.contains("has no download for") && error.contains("set `bin`"),
            "{error}"
        );
    }

    #[test]
    fn the_newest_seed_fallbacks_skip_seeds_lacking_the_host_platform() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let recipe = without_host_seeds(recipe);
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        assert!(pinned_release(&resolver, tool, &recipe, &auto(None)).unwrap().is_none());
        let config = ToolConfig {
            enabled: true,
            version: Some("latest".into()),
            ..ToolConfig::default()
        };
        assert!(pinned_release(&resolver, tool, &recipe, &config).is_err());
    }

    #[test]
    fn without_install_auto_nothing_is_looked_up_and_the_message_says_why() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("tofu");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let config = ToolConfig {
            enabled: true,
            version: Some("0.0.1".into()),
            ..ToolConfig::default()
        };
        let error = pinned_release(&resolver, tool, recipe, &config).expect_err("unresolved");
        assert!(error.contains("not \"auto\"") && error.contains("`tofu`"), "{error}");
        assert_eq!(server.total(), 0);
    }

    #[test]
    fn latest_without_network_falls_back_to_the_newest_seed() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let config = ToolConfig {
            enabled: true,
            version: Some("latest".into()),
            ..ToolConfig::default()
        };
        let pinned = pinned_release(&resolver, tool, recipe, &config).unwrap().unwrap();
        let newest = recipe
            .install
            .as_ref()
            .unwrap()
            .releases
            .keys()
            .max_by_key(|v| resolve::version_key(v))
            .unwrap();
        assert_eq!(&pinned.version, newest);
        assert_eq!(server.total(), 0);
    }

    #[test]
    fn no_version_and_no_auto_install_means_the_path_binary() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("hadolint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let config = ToolConfig {
            enabled: true,
            ..ToolConfig::default()
        };
        assert!(pinned_release(&resolver, tool, recipe, &config).unwrap().is_none());
    }

    #[test]
    fn a_bad_resolve_ttl_is_reported_by_tool() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("hadolint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let mut config = auto(Some("9.9.9"));
        config.resolve_ttl = Some("soon".into());
        let error = pinned_release(&resolver, tool, recipe, &config).expect_err("bad ttl");
        assert!(error.contains("resolve_ttl") && error.contains("hadolint"), "{error}");
    }

    #[test]
    fn bin_wins_without_resolving_any_pin() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("hadolint");
        let exe = std::env::current_exe().unwrap();
        // A version no resolution accepts: were the pin consulted first, this would be Missing.
        let mut config = auto(Some("not a version"));
        config.bin = Some(exe.to_string_lossy().into_owned());
        match resolve(tool, recipe, &config) {
            Resolved::Ready(launcher) => assert_eq!(launcher.program, exe),
            Resolved::Missing(message) => panic!("`bin` must be used as is: {message}"),
        }
        config.bin = Some("/nonexistent/hadolint".into());
        match resolve(tool, recipe, &config) {
            Resolved::Missing(message) => assert!(message.contains("does not exist"), "{message}"),
            Resolved::Ready(_) => panic!("a missing `bin` is missing"),
        }
    }

    #[test]
    fn an_implicit_latest_that_cannot_be_looked_up_runs_the_newest_seed() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let pinned = pinned_release(&resolver, tool, recipe, &auto(None))
            .expect("falls back")
            .expect("to a seed");
        let newest = recipe
            .install
            .as_ref()
            .unwrap()
            .releases
            .keys()
            .max_by_key(|v| resolve::version_key(v))
            .unwrap();
        assert_eq!(&pinned.version, newest);
        assert!(server.total() > 0, "the lookup was attempted first");
    }

    #[test]
    fn an_implicit_latest_without_a_seed_falls_back_to_the_path_binary() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let mut recipe = recipe.clone();
        recipe.install.as_mut().unwrap().releases.clear();
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        assert!(pinned_release(&resolver, tool, &recipe, &auto(None)).unwrap().is_none());
    }

    #[test]
    fn an_explicit_version_that_cannot_be_resolved_never_falls_back() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        for version in ["9.9.9", "latest"] {
            let error = pinned_release(&resolver, tool, recipe, &auto(Some(version))).expect_err(version);
            assert!(error.contains("actionlint"), "{version}: {error}");
        }
    }

    #[test]
    fn a_hostile_version_is_refused_before_any_lookup() {
        let _lock = LockGuard::new();
        let (tool, recipe, _) = setup("actionlint");
        let server = Server::start(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let resolver = Resolver::for_test(&server.base, dir.path().to_path_buf(), false);
        let error = pinned_release(&resolver, tool, recipe, &auto(Some("../../x"))).expect_err("refused");
        assert!(error.contains("not a version"), "{error}");
        assert_eq!(server.total(), 0);
    }
}
