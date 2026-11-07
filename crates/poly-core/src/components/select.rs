//! Entry points for tools outside a run (`poly components`): which components a set of paths
//! needs, and how to open a host over the lock a run would use.

use std::path::PathBuf;

use poly_component_host::HostOptions;
use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_config::BaseConfigResolver;

use super::needed::{feature_is_linked, is_linked, needed_components};
use super::preplan::component_lock;
use crate::config::Config;
use crate::discover::discover;
use crate::language::Language;
use crate::resolve::ConfigSet;

/// Whether every engine family `component` is built from is linked into this binary, so it
/// never needs downloading.
#[must_use]
pub fn component_is_linked(component: &Component) -> bool {
    is_linked(component, &feature_is_linked)
}

/// The names of the downloadable components a run over `paths` would need under `root_config`
/// (nested `poly.toml` files resolved through `resolver`), in index order. Pure discovery: nothing
/// is installed and no host is built.
///
/// # Errors
///
/// When the nested configs under `paths` cannot be resolved.
pub fn needed_component_names(
    index: &ComponentIndex,
    paths: &[PathBuf],
    root_config: &Config,
    resolver: &dyn BaseConfigResolver,
) -> anyhow::Result<Vec<String>> {
    let configs = ConfigSet::build_with(paths, root_config.clone(), resolver)?;
    let files = discover(paths, &configs, &[]);
    let mut seen: Vec<(usize, Language)> = Vec::new();
    for file in &files {
        let pair = (file.config_id, file.language.clone());
        if !seen.contains(&pair) {
            seen.push(pair);
        }
    }
    let all: Vec<&Config> = configs.iter().collect();
    Ok(needed_components(index, &all, &seen, &feature_is_linked, &|_, _| true)
        .into_iter()
        .map(|c| c.name.clone())
        .collect())
}

/// The components of `index` that are sidecar executables, for [`HostOptions::sidecars`].
#[must_use]
pub fn sidecar_names(index: &ComponentIndex) -> Vec<String> {
    index
        .components
        .iter()
        .filter(|c| c.kind == ComponentKind::Sidecar)
        .map(|c| c.name.clone())
        .collect()
}

/// Host options over the lock a run uses (the one embedded at build time; a `dev-lock` build reads `POLY_COMPONENT_DEV_LOCK`)
/// and poly's component cache directory. The caller sets `offline` and the rest.
///
/// # Errors
///
/// When no lock is available or it cannot be read.
pub fn host_options() -> anyhow::Result<HostOptions> {
    let lock = component_lock()?;
    let cache_root = default_cache_root();
    let mut options = HostOptions::new(lock, cache_root);
    options.sidecars = sidecar_names(poly_component_index::index());
    Ok(options)
}

/// The directory components are cached in, before `POLY_COMPONENT_CACHE` overrides it.
#[must_use]
pub fn default_cache_root() -> PathBuf {
    poly_cache::cache_home()
        .map(|home| home.join("components"))
        .unwrap_or_else(|_| std::env::temp_dir().join("poly-components"))
}
