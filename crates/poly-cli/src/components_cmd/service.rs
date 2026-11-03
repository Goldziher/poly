//! The component services a frontend (the MCP server) reaches through the host.
//!
//! These are the same operations `poly components` runs, over the same [`Env`], returned as the
//! wire types of `poly-frontend-api`. Status never downloads; install downloads only what the
//! caller named or the given paths need; uninstall refuses a component this process has loaded.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use poly_component_host::loaded_components;
use poly_component_index::{Component, ComponentIndex};
use poly_config::v2::ComponentInstallPolicy;
use poly_core::components::component_is_linked;
use poly_frontend_api::dto::{ComponentAction, ComponentActions, ComponentRow, ComponentSummary, ComponentsStatus};
use poly_frontend_api::wire::{
    ComponentsInstallRequest, ComponentsStatusRequest, ComponentsUninstallRequest, InstallMode,
};
use poly_host_services::ComponentsProvider;
use sha2::{Digest, Sha256};

use super::ops::{self, Actions, DEFAULT_KEEP_DAYS, Env, Listing, State};
use super::{cache_root_of, component_summaries, needed_with, open_host};

/// Answers the component host services from the core binary's component host.
pub struct HostComponents;

/// Run `f` over an [`Env`] whose host is `offline` or not, with the digest of the lock it was
/// built from. A build with no lock gets an env with no host, which reports why.
fn with_env<R>(offline: bool, f: impl FnOnce(&Env<'_>, Option<String>) -> R) -> R {
    let (host, lock_json, no_host) = match open_host(offline) {
        Ok((host, lock)) => (Some(host), Some(lock), None),
        Err(error) => (None, None, Some(format!("{error:#}"))),
    };
    let lock_sha = lock_json
        .as_deref()
        .map(|lock| super::ops::hex(&Sha256::digest(lock.as_bytes())));
    let env = Env {
        index: poly_component_index::index(),
        host: host.as_ref(),
        no_host,
        lock_json: lock_json.as_deref(),
        linked: &component_is_linked,
        offline: crate::offline(),
        cache_root: cache_root_of(host.as_ref()),
    };
    f(&env, lock_sha)
}

fn to_paths(paths: &[String]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

/// Whether a run under `mode` and the config's `[components] install` may download.
fn may_download(mode: InstallMode, config: ComponentInstallPolicy) -> bool {
    match mode {
        InstallMode::Auto => true,
        InstallMode::Never => false,
        InstallMode::Config => config == ComponentInstallPolicy::Auto,
    }
}

fn status_of(request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus> {
    let paths = to_paths(&request.paths);
    let needed = if paths.is_empty() {
        None
    } else {
        Some(needed_with(&paths, request.config.as_deref().map(Path::new))?)
    };
    Ok(with_env(true, |env, lock_sha| {
        let listing = match &needed {
            Some((names, _)) => ops::status(env, names, false),
            None => ops::list(env),
        };
        let would_download = match &needed {
            Some((_, policy)) if !env.offline && may_download(request.install, policy.install) => listing
                .components
                .iter()
                .filter(|row| row.state == State::NotInstalled)
                .map(|row| row.name.clone())
                .collect(),
            _ => Vec::new(),
        };
        status_reply(listing, lock_sha, would_download)
    }))
}

fn status_reply(listing: Listing, components_lock: Option<String>, would_download: Vec<String>) -> ComponentsStatus {
    ComponentsStatus {
        flavor: listing.flavor.to_owned(),
        offline: listing.offline,
        cache_root: listing.cache_root,
        components_lock,
        lock_error: listing.lock,
        components: listing
            .components
            .into_iter()
            .map(|row| ComponentRow {
                name: row.name,
                kind: row.kind.to_owned(),
                state: row.state.label().to_owned(),
                opt_in: row.opt_in,
                engines: row.engines,
                needed: row.needed,
                size_bytes: row.size_bytes,
                detail: row.detail,
            })
            .collect(),
        missing: listing.missing,
        would_download,
    }
}

fn actions_reply(actions: Actions) -> ComponentActions {
    ComponentActions {
        command: actions.command.to_owned(),
        offline: actions.offline,
        ok: actions.ok,
        results: actions
            .results
            .into_iter()
            .map(|row| ComponentAction {
                name: row.name,
                action: row.action.to_owned(),
                detail: row.detail,
            })
            .collect(),
        freed_bytes: actions.freed_bytes,
        notes: actions.notes,
    }
}

/// The components `names` (names, presets) and `languages` select, in the order given.
///
/// A language is a preset alias (`python`) or, failing that, a language id some engine serves.
fn select<'a>(index: &'a ComponentIndex, names: &[String], languages: &[String]) -> anyhow::Result<Vec<&'a Component>> {
    let mut picked = ops::resolve(index, names)?;
    for language in languages {
        let found = match index.expand_preset(language) {
            Some(found) => found,
            None => index.components_for_language(language),
        };
        if found.is_empty() {
            bail!("no component serves the language `{language}`");
        }
        for component in found {
            if !picked.iter().any(|p| p.name == component.name) {
                picked.push(component);
            }
        }
    }
    Ok(picked)
}

/// What an install would do, without doing it.
fn plan_install(env: &Env<'_>, components: &[&Component]) -> Actions {
    let mut actions = Actions::new("install", env);
    for component in components {
        match ops::state_of(env, component) {
            (State::Bundled, _) => actions.push(&component.name, "bundled", None),
            (State::Installed, _) => actions.push(&component.name, "already_installed", None),
            (State::NotInstalled, _) if env.offline => {
                actions.push(
                    &component.name,
                    "failed",
                    Some("offline: the component is not installed".into()),
                );
            }
            (State::NotInstalled, _) => actions.push(&component.name, "would_install", None),
            (State::Unavailable, detail) => actions.push(&component.name, "failed", detail),
        }
    }
    actions
}

fn install_of(request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions> {
    let paths = to_paths(&request.paths);
    anyhow::ensure!(
        !(request.names.is_empty() && request.languages.is_empty() && paths.is_empty()),
        "name at least one of `names`, `languages` or `paths`"
    );
    let needed = if paths.is_empty() {
        Vec::new()
    } else {
        needed_with(&paths, request.config.as_deref().map(Path::new))?.0
    };
    with_env(crate::offline(), |env, _| {
        let mut picked = select(env.index, &request.names, &request.languages)?;
        for name in &needed {
            if let Some(component) = env.index.component(name)
                && !picked.iter().any(|p| p.name == component.name)
            {
                picked.push(component);
            }
        }
        let actions = if request.dry_run {
            plan_install(env, &picked)
        } else {
            ops::install(env, &picked)?
        };
        Ok(actions_reply(actions))
    })
}

fn uninstall_of(request: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions> {
    anyhow::ensure!(
        !request.names.is_empty() || request.stale,
        "name at least one component, or set `stale`"
    );
    // Removal never needs the network, so the host is built offline whatever the run is.
    with_env(true, |env, _| {
        let selected = ops::resolve(env.index, &request.names)?;
        let loaded = loaded_components();
        let (refused, removable): (Vec<&Component>, Vec<&Component>) =
            selected.into_iter().partition(|c| loaded.contains(&c.name));
        let mut actions = if removable.is_empty() {
            Actions::new("uninstall", env)
        } else {
            ops::uninstall(env, &removable)?
        };
        for component in refused {
            actions.push(
                &component.name,
                "refused",
                Some("loaded in this process; restart poly to remove it".into()),
            );
            actions.ok = false;
        }
        if request.stale {
            let pruned = ops::prune(env, false, false, DEFAULT_KEEP_DAYS).context("cannot remove stale components")?;
            actions.ok &= pruned.ok;
            actions.results.extend(pruned.results);
            actions.freed_bytes = Some(actions.freed_bytes.unwrap_or(0) + pruned.freed_bytes.unwrap_or(0));
            actions.notes.extend(pruned.notes);
        }
        Ok(actions_reply(actions))
    })
}

impl ComponentsProvider for HostComponents {
    fn components(&self) -> anyhow::Result<Vec<ComponentSummary>> {
        Ok(component_summaries())
    }

    fn identity(&self) -> (String, Option<String>) {
        with_env(true, |env, lock_sha| (ops::flavor(env).to_owned(), lock_sha))
    }

    fn status(&self, request: &ComponentsStatusRequest) -> anyhow::Result<ComponentsStatus> {
        status_of(request)
    }

    fn install(&self, request: &ComponentsInstallRequest) -> anyhow::Result<ComponentActions> {
        install_of(request)
    }

    fn uninstall(&self, request: &ComponentsUninstallRequest) -> anyhow::Result<ComponentActions> {
        uninstall_of(request)
    }
}
