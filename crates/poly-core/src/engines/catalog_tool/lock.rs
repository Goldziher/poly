//! The tool half of `poly lock`: which `[tools]` entries are lockable, and the
//! release (version plus every platform's URL and SHA-256) each resolves to.
//!
//! A tool is lockable when it is enabled, may be downloaded (`install = "auto"`),
//! has a release source in the catalog, and does not already name its own `url`
//! and `sha256`. The lock then fixes what `latest` means, so a run with a lock
//! entry does no resolution I/O and CI is reproducible.

use std::time::Duration;

use poly_catalog::{Catalog, Source};
use poly_config::tool_lock::{LockedAsset, LockedTool};
use poly_config::{InstallPolicy, ToolConfig, ToolsConfig};

use super::recipe::resolve::cache::{self, DEFAULT_TTL};
use super::recipe::resolve::{Network, Request, Resolver, Scope};

fn source_of(name: &str) -> Option<&'static Source> {
    Catalog::get()
        .tool(name)?
        .recipe
        .as_ref()?
        .install
        .as_ref()?
        .source
        .as_ref()
}

/// Names of the configured tools `poly lock` pins, in name order.
pub fn lockable_tools(tools: &ToolsConfig) -> Vec<String> {
    tools
        .iter()
        .filter(|(name, config)| {
            config.enabled
                && config.install == InstallPolicy::Auto
                && !(config.url.is_some() && config.sha256.is_some())
                && source_of(name).is_some()
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Whether `locked` already answers what `config` asks of `name`: any lock
/// serves `latest`, and an exact `version` needs that very version.
pub fn lock_satisfies(config: &ToolConfig, locked: &LockedTool) -> bool {
    match Request::of(config.version.as_deref()) {
        Ok(Request::Latest) => true,
        Ok(Request::Exact(version)) => version == locked.version,
        Err(_) => false,
    }
}

/// The lock entry for the tool `name`.
///
/// An `existing` entry that still satisfies the config and covers every
/// platform the source publishes is kept without asking anyone, unless `upgrade`
/// asks for the newest release. `upgrade` leaves an exact `version` where it is:
/// the digests of a version already locked are not fetched again, and a
/// re-published asset is refused only when a re-resolution (a missing platform,
/// or the entry removed) finds it changed.
///
/// # Errors
///
/// The message says what was attempted and what to do: no source, offline, an
/// unknown version, a rate limit.
pub fn lock_tool(
    name: &str,
    config: &ToolConfig,
    upgrade: bool,
    existing: Option<&LockedTool>,
) -> Result<LockedTool, String> {
    let Some(source) = source_of(name) else {
        return Err(format!("tool `{name}` has no release source to lock"));
    };
    if let Some(kept) = existing
        && !upgrade
        && lock_satisfies(config, kept)
        && covers_platforms(source, kept)
    {
        return Ok(kept.clone());
    }
    let request = Request::of(config.version.as_deref()).map_err(|message| format!("tool `{name}`: {message}"))?;
    let ttl = match (&request, upgrade) {
        (Request::Latest, true) => Duration::ZERO,
        _ => match config.resolve_ttl.as_deref() {
            Some(text) => cache::parse_ttl(text).map_err(|error| format!("tool `{name}`: {error:#}"))?,
            None => DEFAULT_TTL,
        },
    };
    let release = Resolver::shared().resolve(name, source, &request, ttl, Scope::All, Network::Allowed)?;
    let mut assets: Vec<LockedAsset> = release
        .assets
        .iter()
        .map(|(platform, asset)| LockedAsset {
            platform: platform.clone(),
            url: asset.url.clone(),
            sha256: asset.sha256.clone(),
            member: asset.member.clone(),
        })
        .collect();
    assets.sort_by(|a, b| a.platform.cmp(&b.platform));
    refuse_republished(name, existing, &assets)?;
    Ok(LockedTool {
        name: name.to_owned(),
        version: release.version.clone(),
        assets,
    })
}

/// Whether `locked` has an asset for every platform `source` publishes, so a
/// platform added to the catalog is picked up by a plain `poly lock`.
fn covers_platforms(source: &Source, locked: &LockedTool) -> bool {
    source
        .assets
        .keys()
        .all(|platform| locked.assets.iter().any(|asset| &asset.platform == platform))
}

/// An asset of the version already locked must keep its digest: a changed one
/// means upstream re-published the file, which poly will not accept silently.
fn refuse_republished(name: &str, existing: Option<&LockedTool>, assets: &[LockedAsset]) -> Result<(), String> {
    let Some(kept) = existing else {
        return Ok(());
    };
    let changed = kept.assets.iter().find(|old| {
        assets
            .iter()
            .any(|new| new.platform == old.platform && new.url == old.url && new.sha256 != old.sha256)
    });
    match changed {
        Some(old) => Err(format!(
            "tool `{name}` {} was re-published upstream: the {} asset no longer has the locked SHA-256 {}. \
             Refusing to move the lock; verify the new file, then remove the entry from poly.lock and run `poly lock`",
            kept.version, old.platform, old.sha256
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(version: &str, digest: char) -> LockedTool {
        LockedTool {
            name: "t".into(),
            version: version.into(),
            assets: vec![LockedAsset {
                platform: "linux-x86_64".into(),
                url: format!("https://example.invalid/t-{version}"),
                sha256: digest.to_string().repeat(64),
                member: None,
            }],
        }
    }

    #[test]
    fn a_re_published_asset_is_refused_and_a_new_version_is_not() {
        let old = locked("1.0.0", 'a');
        let err = refuse_republished("t", Some(&old), &locked("1.0.0", 'b').assets).unwrap_err();
        assert!(err.contains("re-published") && err.contains("linux-x86_64"), "{err}");
        refuse_republished("t", Some(&old), &locked("1.0.0", 'a').assets).expect("same digest");
        refuse_republished("t", Some(&old), &locked("1.1.0", 'b').assets).expect("another version, another url");
        refuse_republished("t", None, &locked("1.0.0", 'b').assets).expect("nothing locked");
    }

    #[test]
    fn an_entry_missing_a_published_platform_is_not_kept() {
        let source: Source = serde_json::from_value(serde_json::json!({
            "kind": "binary", "github": "o/r",
            "assets": {"linux-x86_64": {"name": "t-{version}"}, "windows-x86_64": {"name": "t-{version}.zip"}}
        }))
        .unwrap();
        let entry = locked("1.0.0", 'a');
        assert!(!covers_platforms(&source, &entry));
        let mut full = entry.clone();
        full.assets.push(LockedAsset {
            platform: "windows-x86_64".into(),
            ..entry.assets[0].clone()
        });
        assert!(covers_platforms(&source, &full));
    }

    #[test]
    fn an_exact_version_is_satisfied_only_by_itself_and_latest_by_any() {
        let entry = locked("1.0.0", 'a');
        let with = |version: Option<&str>| ToolConfig {
            version: version.map(str::to_owned),
            ..ToolConfig::default()
        };
        assert!(lock_satisfies(&with(None), &entry));
        assert!(lock_satisfies(&with(Some("latest")), &entry));
        assert!(lock_satisfies(&with(Some("v1.0.0")), &entry));
        assert!(!lock_satisfies(&with(Some("1.0.1")), &entry));
    }

    #[test]
    fn only_enabled_auto_installed_tools_with_a_source_are_lockable() {
        let parse = |text: &str| -> ToolsConfig { toml::from_str(text).expect("tools") };
        let tools = parse(
            r#"
            [hadolint]
            enabled = true
            install = "auto"
            [tflint]
            enabled = true
            [tofu]
            install = "auto"
            [buf]
            enabled = true
            install = "auto"
            url = "https://x/buf"
            sha256 = "ab"
            version = "1.0.0"
            [shfmt]
            enabled = true
            install = "auto"
            "#,
        );
        assert_eq!(lockable_tools(&tools), ["hadolint"]);
    }
}
