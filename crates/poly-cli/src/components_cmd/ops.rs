//! What each `poly components` subcommand does, over an [`Env`] so tests can drive it with a fake
//! connector and a temporary cache.
//!
//! `list` and `status` only ever call [`ComponentHost::status`] (a file-existence check) and, for
//! `--verify`, [`ComponentManager::prefetch`] on a manager built offline; they never reach
//! [`ComponentHost::ensure`] or a connector, so they cannot download. Installing is confined to
//! [`install`] and [`sync`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use alef_component_runtime::{ArtifactCache, ComponentLock, ComponentStatus, TrustPolicy};
use anyhow::{Context, bail};
use poly_component_host::{Availability, ComponentHost};
use poly_component_index::{Component, ComponentIndex, ComponentKind};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::prune;

/// Version of the `--format json|toon` documents.
pub const SCHEMA: u32 = 1;

/// Days a cached archive no lock references survives `prune` without `--all`.
pub const DEFAULT_KEEP_DAYS: u64 = 30;

/// Everything a subcommand reads.
pub struct Env<'a> {
    /// The component index.
    pub index: &'a ComponentIndex,
    /// A host over the component lock, when there is one.
    pub host: Option<&'a ComponentHost>,
    /// Why there is no host, when there is none.
    pub no_host: Option<String>,
    /// The lock text the host was built from, for the trust keys `import` needs.
    pub lock_json: Option<&'a str>,
    /// Whether a component's families are linked into this binary.
    pub linked: &'a dyn Fn(&Component) -> bool,
    /// Whether the run is offline.
    pub offline: bool,
    /// Where components are cached.
    pub cache_root: PathBuf,
}

impl Env<'_> {
    fn host(&self) -> anyhow::Result<&ComponentHost> {
        self.host.ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                self.no_host
                    .as_deref()
                    .unwrap_or("no component lock is available for this build")
            )
        })
    }
}

/// Where a component stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Linked into this binary.
    Bundled,
    /// Downloaded and cached.
    Installed,
    /// Published for this platform but not downloaded.
    NotInstalled,
    /// Not in the lock for this platform, or there is no lock.
    Unavailable,
}

impl State {
    /// The word reports print.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Installed => "installed",
            Self::NotInstalled => "not installed",
            Self::Unavailable => "unavailable",
        }
    }

    const fn usable(self) -> bool {
        matches!(self, Self::Bundled | Self::Installed)
    }
}

/// One component in a [`Listing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentRow {
    /// Component name.
    pub name: String,
    /// `engines` or `frontend`.
    pub kind: &'static str,
    /// Where it stands.
    pub state: State,
    /// Installed only when asked for, never by the `all` preset.
    pub opt_in: bool,
    /// The engines it hosts.
    pub engines: Vec<String>,
    /// Whether the paths asked about need it (`status` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needed: Option<bool>,
    /// Size on disk of the installed archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Where the installed archive lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Whether the installed archive passed re-verification (`status --verify`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Why it is unavailable, or why verification failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The document `list` and `status` print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    /// Document version.
    pub schema: u32,
    /// `list` or `status`.
    pub command: &'static str,
    /// `bundled` (every engine component linked), `core` (none) or `partial`.
    pub flavor: &'static str,
    /// Whether the run is offline.
    pub offline: bool,
    /// Where components are cached.
    pub cache_root: String,
    /// Why no component lock is available, when none is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// The components.
    pub components: Vec<ComponentRow>,
    /// Needed components that are not usable (`status` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing: Option<Vec<String>>,
}

impl Listing {
    /// Whether `status` found everything the paths need usable.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.missing.as_ref().is_none_or(Vec::is_empty) && self.components.iter().all(|c| c.verified != Some(false))
    }
}

/// One outcome in an [`Actions`] document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionRow {
    /// The component, or the cached object a prune names.
    pub name: String,
    /// What happened: `installed`, `already_installed`, `bundled`, `removed`, `not_installed`,
    /// `would_remove`, `skipped`, `imported` or `failed`.
    pub action: &'static str,
    /// The reason, for a failure or a skipped removal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The document `install`, `uninstall`, `sync`, `prune` and `import` print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Actions {
    /// Document version.
    pub schema: u32,
    /// The subcommand.
    pub command: &'static str,
    /// Whether the run is offline.
    pub offline: bool,
    /// Whether every component came out usable (or removed, for the removing commands).
    pub ok: bool,
    /// What happened to each.
    pub results: Vec<ActionRow>,
    /// Bytes a prune or uninstall freed (or would free, under `--dry-run`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freed_bytes: Option<u64>,
    /// Remarks for a human.
    pub notes: Vec<String>,
}

impl Actions {
    fn new(command: &'static str, env: &Env<'_>) -> Self {
        Self {
            schema: SCHEMA,
            command,
            offline: env.offline,
            ok: true,
            results: Vec::new(),
            freed_bytes: None,
            notes: Vec::new(),
        }
    }

    fn push(&mut self, name: &str, action: &'static str, detail: Option<String>) {
        if action == "failed" {
            self.ok = false;
        }
        self.results.push(ActionRow {
            name: name.to_owned(),
            action,
            detail,
        });
    }
}

/// `bundled` when every engine component is linked, `core` when none is, else `partial`.
#[must_use]
pub fn flavor(env: &Env<'_>) -> &'static str {
    let engines: Vec<&Component> = env
        .index
        .components
        .iter()
        .filter(|c| c.kind == ComponentKind::Engines)
        .collect();
    let linked = engines.iter().filter(|c| (env.linked)(c)).count();
    if linked == 0 {
        "core"
    } else if linked == engines.len() {
        "bundled"
    } else {
        "partial"
    }
}

fn engine_names(component: &Component) -> Vec<String> {
    let names: BTreeSet<&str> = component.engines.iter().map(|e| e.name.as_str()).collect();
    names.into_iter().map(str::to_owned).collect()
}

fn state_of(env: &Env<'_>, component: &Component) -> (State, Option<String>) {
    if (env.linked)(component) {
        return (State::Bundled, None);
    }
    let Some(host) = env.host else {
        return (State::Unavailable, env.no_host.clone());
    };
    match host.status(&component.name) {
        Ok(ComponentStatus::Cached | ComponentStatus::Ready) => (State::Installed, None),
        Ok(ComponentStatus::Bundled) => (State::Bundled, None),
        Ok(ComponentStatus::NotDownloaded) => (State::NotInstalled, None),
        Ok(ComponentStatus::Unsupported { reason }) => (State::Unavailable, Some(reason)),
        Err(error) => (State::Unavailable, Some(format!("{error:#}"))),
    }
}

fn row(env: &Env<'_>, component: &Component, cached: &[alef_component_runtime::CachedObject]) -> ComponentRow {
    let (state, detail) = state_of(env, component);
    let object = (state == State::Installed)
        .then(|| {
            cached
                .iter()
                .find(|o| o.component.as_deref() == Some(component.name.as_str()))
        })
        .flatten();
    ComponentRow {
        name: component.name.clone(),
        kind: match component.kind {
            ComponentKind::Engines => "engines",
            ComponentKind::Frontend => "frontend",
        },
        state,
        opt_in: component.opt_in,
        engines: engine_names(component),
        needed: None,
        size_bytes: object.map(|o| o.size_bytes),
        path: object.map(|o| o.path.clone()),
        verified: None,
        detail,
    }
}

fn cached_objects(env: &Env<'_>) -> Vec<alef_component_runtime::CachedObject> {
    env.host
        .and_then(|host| host.manager().list_cached().ok())
        .unwrap_or_default()
}

fn listing(
    env: &Env<'_>,
    command: &'static str,
    components: Vec<ComponentRow>,
    missing: Option<Vec<String>>,
) -> Listing {
    Listing {
        schema: SCHEMA,
        command,
        flavor: flavor(env),
        offline: env.offline,
        cache_root: env.cache_root.display().to_string(),
        lock: env.no_host.clone(),
        components,
        missing,
    }
}

/// Every component in the index and where it stands. Never downloads.
#[must_use]
pub fn list(env: &Env<'_>) -> Listing {
    let cached = cached_objects(env);
    let rows = env.index.components.iter().map(|c| row(env, c, &cached)).collect();
    listing(env, "list", rows, None)
}

/// The components `needed` names and where they stand; with `verify`, re-check each installed
/// archive against the lock. Never downloads: the host is offline-only for this command.
#[must_use]
pub fn status(env: &Env<'_>, needed: &[String], verify: bool) -> Listing {
    let cached = cached_objects(env);
    let mut rows = Vec::new();
    for component in env.index.components.iter().filter(|c| needed.contains(&c.name)) {
        let mut row = row(env, component, &cached);
        row.needed = Some(true);
        if verify && row.state == State::Installed {
            match env.host.map(|host| host.manager().prefetch(&[component.name.as_str()])) {
                Some(Ok(_)) => row.verified = Some(true),
                Some(Err(error)) => {
                    row.verified = Some(false);
                    row.detail = Some(format!("{error:#}"));
                }
                None => {}
            }
        }
        rows.push(row);
    }
    let missing = rows
        .iter()
        .filter(|r| !r.state.usable())
        .map(|r| r.name.clone())
        .collect();
    listing(env, "status", rows, Some(missing))
}

/// The components a `<name|preset|all>` argument selects.
///
/// # Errors
///
/// When a name is neither a component nor a preset.
pub fn resolve<'a>(index: &'a ComponentIndex, names: &[String]) -> anyhow::Result<Vec<&'a Component>> {
    let mut picked: Vec<&Component> = Vec::new();
    for name in names {
        let Some(expanded) = index.expand_preset(name) else {
            let mut known: Vec<&str> = index.components.iter().map(|c| c.name.as_str()).collect();
            known.extend(index.presets.keys().map(String::as_str));
            bail!(
                "`{name}` is neither a component nor a preset (known: {})",
                known.join(", ")
            );
        };
        for component in expanded {
            if !picked.iter().any(|p| p.name == component.name) {
                picked.push(component);
            }
        }
    }
    Ok(picked)
}

fn availability_row(actions: &mut Actions, name: &str, availability: &Availability) {
    match availability {
        Availability::Present => actions.push(name, "already_installed", None),
        Availability::InstalledNow => actions.push(name, "installed", None),
        Availability::Unavailable(unavailable) => actions.push(name, "failed", Some(unavailable.reason(name))),
    }
}

/// Install `components`. Linked ones are a no-op with a note; an offline run fails the missing.
///
/// # Errors
///
/// When a component needs installing and there is no lock.
pub fn install(env: &Env<'_>, components: &[&Component]) -> anyhow::Result<Actions> {
    let mut actions = Actions::new("install", env);
    for component in components {
        if (env.linked)(component) {
            actions.push(&component.name, "bundled", None);
            continue;
        }
        let availability = env.host()?.ensure(&component.name, true);
        availability_row(&mut actions, &component.name, &availability);
    }
    if actions.results.iter().any(|r| r.action == "bundled") {
        actions
            .notes
            .push("this build links these engines in; there is nothing to install for them".into());
    }
    Ok(actions)
}

/// Install every downloadable component `needed` names; offline, only check that they are there.
///
/// # Errors
///
/// When something needs installing and there is no lock.
pub fn sync(env: &Env<'_>, needed: &[String]) -> anyhow::Result<Actions> {
    let mut actions = Actions::new("sync", env);
    if needed.is_empty() {
        actions.notes.push("no downloadable component is needed".into());
    }
    for name in needed {
        let availability = env.host()?.ensure(name, true);
        availability_row(&mut actions, name, &availability);
    }
    actions
        .notes
        .push("only components are synced: grammar and tool downloads have no standalone entry point yet".into());
    Ok(actions)
}

/// Remove the cached archives of `components`.
///
/// # Errors
///
/// When there is no lock, or the cache cannot be modified.
pub fn uninstall(env: &Env<'_>, components: &[&Component]) -> anyhow::Result<Actions> {
    let mut actions = Actions::new("uninstall", env);
    let mut freed = 0;
    for component in components {
        if (env.linked)(component) {
            actions.push(&component.name, "bundled", None);
            continue;
        }
        let report = env.host()?.manager().remove(&component.name)?;
        freed += report.freed_bytes;
        let skipped = report
            .skipped
            .iter()
            .map(|s| format!("{}: {}", s.path.display(), s.reason))
            .collect::<Vec<_>>();
        if !skipped.is_empty() {
            actions.push(&component.name, "failed", Some(skipped.join("; ")));
        } else if report.removed.is_empty() {
            actions.push(&component.name, "not_installed", None);
        } else {
            actions.push(&component.name, "removed", None);
        }
    }
    actions.freed_bytes = Some(freed);
    Ok(actions)
}

/// Remove cached archives the lock no longer references and that nothing used within `keep_days`,
/// plus orphaned staging directories; with `all`, remove every archive. `dry_run` only reports.
///
/// The dry run and the real prune execute one [`prune::plan`], so they name the same paths. An
/// archive the lock references for any target is kept; one only an older poly version's lock names
/// is removed once stale. A removal the OS refuses because a loaded library holds the file is
/// reported as `skipped` and left for a later prune.
///
/// # Errors
///
/// When there is no lock, or the cache cannot be read or modified.
pub fn prune(env: &Env<'_>, all: bool, dry_run: bool, keep_days: u64) -> anyhow::Result<Actions> {
    prune_with(env, all, dry_run, keep_days, &prune::remove_path)
}

/// [`prune`] with the removal injected, so a test can simulate a file the OS will not delete.
///
/// # Errors
///
/// As [`prune`].
pub fn prune_with(
    env: &Env<'_>,
    all: bool,
    dry_run: bool,
    keep_days: u64,
    remover: &dyn Fn(&Path) -> std::io::Result<()>,
) -> anyhow::Result<Actions> {
    let host = env.host()?;
    let mut actions = Actions::new("prune", env);
    let objects = host.manager().list_cached()?;
    let referenced = prune::locked_digests(env.lock_json);
    let candidates = prune::plan(
        &env.cache_root,
        &objects,
        &referenced,
        all,
        keep_days,
        SystemTime::now(),
    )
    .context("cannot read the component cache")?;
    if dry_run {
        let mut freed = 0;
        for candidate in &candidates {
            freed += candidate.size_bytes;
            actions.push(&path_name(&candidate.path), "would_remove", candidate.component.clone());
        }
        actions.freed_bytes = Some(freed);
        return Ok(actions);
    }
    let outcome = prune::execute(candidates, remover).context("cannot remove from the component cache")?;
    let mut freed = 0;
    for candidate in &outcome.removed {
        freed += candidate.size_bytes;
        actions.push(&path_name(&candidate.path), "removed", candidate.component.clone());
    }
    for (candidate, reason) in &outcome.skipped {
        actions.push(&path_name(&candidate.path), "skipped", Some(reason.clone()));
    }
    if !outcome.skipped.is_empty() {
        actions
            .notes
            .push("skipped paths are in use and stay until a later prune".into());
    }
    actions.freed_bytes = Some(freed);
    Ok(actions)
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Install a component archive from disk, verified against the lock like a download, so an
/// air-gapped machine can be seeded. The archive is matched to its component by digest.
///
/// alef's manager has no import call; this opens its [`ArtifactCache`] at the manager's cache root
/// with the lock's trust keys and installs through `install_from_reader`.
///
/// # Errors
///
/// When there is no lock, the archive cannot be read, matches no component in the lock, or fails
/// verification.
pub fn import(env: &Env<'_>, archive: &Path) -> anyhow::Result<Actions> {
    let host = env.host()?;
    let lock_json = env.lock_json.context("no component lock is available for this build")?;
    let bytes = std::fs::read(archive).with_context(|| format!("cannot read {}", archive.display()))?;
    let digest = hex(&Sha256::digest(&bytes));
    let manager = host.manager();
    let entry = manager
        .lock_entries()
        .into_iter()
        .find(|entry| entry.sha256 == digest)
        .with_context(|| format!("the archive's sha256 {digest} matches no component in the lock for this platform"))?;
    let lock: ComponentLock = serde_json::from_str(lock_json).context("the component lock is not valid")?;
    ArtifactCache::new(manager.cache_root(), TrustPolicy::EmbeddedKeys(lock.public_keys))
        .install_from_reader(entry, bytes.as_slice())
        .with_context(|| format!("{} failed verification", archive.display()))?;
    let mut actions = Actions::new("import", env);
    actions.push(&entry.identity.component, "imported", None);
    Ok(actions)
}
