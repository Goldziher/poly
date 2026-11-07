//! The pre-plan pass: make every needed component usable, or record why it is not.

use std::collections::BTreeMap;
use std::sync::Arc;

use poly_component_host::{Availability, ComponentEngine, ComponentHost, HostOptions, Unavailable, UnavailableKind};
use poly_component_index::{Component, ComponentIndex};
use poly_component_protocol::{Describe, PROTOCOL_VERSION};
use poly_config::network;
use poly_config::v2::{ComponentInstallPolicy, OnMissing};
use rustc_hash::{FxHashMap, FxHashSet};

use super::seam::ComponentInjection;
#[cfg(feature = "dev-lock")]
use super::seam::LOCK_ENV;
use super::{
    ComponentsReport, MissingComponent, feature_is_linked, needed_components_with_ancestors, selected_components,
    selected_whole_tree_components,
};
use crate::config::Config;
use crate::discover::DiscoveredFile;
use crate::engine::Engine;
use crate::language::Language;
use crate::resolve::ConfigSet;
use crate::runner::RunOptions;

/// Digest shown in the version of an engine whose component has no lock entry.
const NO_LOCK_DIGEST: &str = "0000000000000000";

/// What the pre-plan pass hands the planner: the component engines of every `(config,
/// language)` pair, and the run's account of its components.
#[derive(Default)]
pub(crate) struct ResolvedComponents {
    by_pair: FxHashMap<(usize, Language), Vec<ComponentEngine>>,
    report: ComponentsReport,
}

impl ResolvedComponents {
    /// The component engines that serve `language` under `config_id`: real proxies for usable
    /// components, stand-ins for the rest. Empty for a run that needed no component.
    pub(crate) fn engines(&self, config_id: usize, language: &Language) -> &[ComponentEngine] {
        self.by_pair
            .get(&(config_id, language.clone()))
            .map_or(&[], Vec::as_slice)
    }

    /// The run's account of its components.
    pub(crate) fn report(&self) -> &ComponentsReport {
        &self.report
    }
}

#[cfg(test)]
impl ResolvedComponents {
    /// Components serving one `(config, language)` pair with `engines`, for planner tests.
    pub(crate) fn serving(config_id: usize, language: Language, engines: Vec<ComponentEngine>) -> Self {
        Self {
            by_pair: FxHashMap::from_iter([((config_id, language), engines)]),
            report: ComponentsReport::default(),
        }
    }
}

/// How the host was obtained.
enum Hosted {
    Host(Arc<ComponentHost>, bool),
    /// No host could be built; every component is unavailable with this reason.
    Unavailable(String),
}

/// The lock this build embeds (`POLY_COMPONENT_LOCK_FILE` at build time); an error for a build
/// that embeds none.
#[cfg(not(feature = "dev-lock"))]
pub(super) fn component_lock() -> anyhow::Result<String> {
    super::embedded::embedded_lock()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("this build embeds no component lock"))
}

/// The lock a development build reads from `LOCK_ENV` in place of an embedded one.
#[cfg(feature = "dev-lock")]
pub(super) fn component_lock() -> anyhow::Result<String> {
    let path = std::env::var_os(LOCK_ENV).ok_or_else(|| {
        anyhow::anyhow!("this build embeds no component lock (set {LOCK_ENV} for a development lock)")
    })?;
    std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("cannot read the component lock {path:?}: {e}"))
}

/// A component whose index entry or load is unusable; always blocking, since no policy turns a
/// broken component into a skip.
fn incompatible(name: &str, reason: String) -> MissingComponent {
    MissingComponent {
        name: name.to_owned(),
        kind: UnavailableKind::Incompatible.label().to_owned(),
        reason,
        blocking: true,
    }
}

fn open_host(injected: Option<&ComponentInjection>, index: &ComponentIndex) -> Hosted {
    if let Some(injected) = injected {
        return Hosted::Host(Arc::clone(&injected.host), false);
    }
    let built = component_lock().and_then(|lock| {
        let mut options = HostOptions::new(lock, super::default_cache_root());
        options.offline = network::offline();
        options.gate_loads = true;
        options.sidecars = super::sidecar_names(index);
        ComponentHost::new(options)
    });
    match built {
        Ok(host) => Hosted::Host(Arc::new(host), true),
        Err(error) => Hosted::Unavailable(format!("{error:#}")),
    }
}

/// What the index says a component's `describe()` must answer. The index carries no component
/// version, so the slot compares only the protocol and the engines.
pub(super) fn expected_describe(component: &Component) -> Describe {
    Describe {
        protocol: PROTOCOL_VERSION,
        component_version: String::new(),
        engines: component.engines.clone(),
    }
}

/// Resolve the components `files` need under `configs`, and the ones the `ancestors` configs need for
/// the languages of `files`.
///
/// This is the only pass that installs anything. It runs before planning and outside the
/// workers, ensures each needed component once, then seals the host it built. The policy is the
/// root config's `[components] install` / `on_missing` (`opts.on_missing` and `opts.install`, when set, win), and `--offline`.
///
/// A component that is not installed because the run is offline or `install = "never"` becomes a
/// stand-in that skips its files (charged to the skip budget); `on_missing = "fail"` still fails
/// the run, through [`ComponentsReport::blocking`]. A component whose install was attempted and
/// failed becomes a stand-in that errors on each of its files.
pub(crate) fn resolve(
    files: &[DiscoveredFile],
    configs: &ConfigSet,
    opts: &RunOptions,
    ancestors: &[usize],
) -> ResolvedComponents {
    let injected = opts.component_injection.as_ref();
    let index: &ComponentIndex = super::seam::run_index(opts);
    let mut seen: FxHashSet<(usize, Language)> = FxHashSet::default();
    let mut pairs = Vec::new();
    for file in files {
        let pair = (file.config_id, file.language.clone());
        if seen.insert(pair.clone()) {
            pairs.push(pair);
        }
    }
    // A config above a named path owns no file of the run, yet its whole-tree project engines are planned
    // for every language the run saw.
    let languages: FxHashSet<Language> = files.iter().map(|file| file.language.clone()).collect();
    let mut ancestor_pairs = Vec::new();
    for config_id in crate::runner::without_owning(ancestors.to_vec(), files) {
        for language in &languages {
            let pair = (config_id, language.clone());
            if seen.insert(pair.clone()) {
                ancestor_pairs.push(pair);
            }
        }
    }
    let all: Vec<&Config> = configs.iter().collect();
    let selection = crate::runner::selection::EngineSelection::new(&opts.only, &opts.skip);
    let selected = |name: &str, table: Option<&str>| selection.selects_declared(name, table);
    let needed = needed_components_with_ancestors(index, &all, &pairs, &ancestor_pairs, &feature_is_linked, &selected);
    if needed.is_empty() {
        return ResolvedComponents::default();
    }

    let root = &configs.config(0).components;
    let on_missing = opts.on_missing.unwrap_or(root.on_missing);
    let allow_install = opts.install.unwrap_or(root.install) == ComponentInstallPolicy::Auto;
    let hosted = open_host(injected, index);

    let mut report = ComponentsReport::default();
    let mut engines: BTreeMap<&str, Vec<ComponentEngine>> = BTreeMap::new();
    for component in needed {
        let name = component.name.as_str();
        let availability = match &hosted {
            Hosted::Host(host, _) => host.ensure(name, allow_install),
            Hosted::Unavailable(detail) => Availability::Unavailable(Unavailable {
                kind: UnavailableKind::Incompatible,
                detail: detail.clone(),
            }),
        };
        let sha = match &hosted {
            Hosted::Host(host, _) => host.lock_sha256(name),
            Hosted::Unavailable(_) => None,
        }
        .unwrap_or_else(|| NO_LOCK_DIGEST.to_owned());
        let mut built = Vec::new();
        match &availability {
            Availability::Present | Availability::InstalledNow => {
                report.used.push(name.to_owned());
                if availability == Availability::InstalledNow {
                    tracing::info!(component = name, "installed component");
                    report.installed_now.push(name.to_owned());
                }
                if let Hosted::Host(host, _) = &hosted {
                    let mut first_error = None;
                    for desc in &component.engines {
                        match host.engine(name, desc, Some(expected_describe(component))) {
                            Ok(engine) => built.push(engine),
                            Err(error) => {
                                let reason = format!("incompatible: {name}: {error:#}");
                                if let Ok(stub) = ComponentEngine::unavailable_failing(name, desc, &sha, reason.clone())
                                {
                                    built.push(stub);
                                }
                                first_error.get_or_insert(reason);
                            }
                        }
                    }
                    if let Some(reason) = first_error {
                        report.missing.push(incompatible(name, reason));
                    }
                }
            }
            Availability::Unavailable(unavailable) => {
                let reason = unavailable.reason(name);
                let skips = unavailable.kind.is_not_installed();
                report.missing.push(MissingComponent {
                    name: name.to_owned(),
                    kind: unavailable.kind.label().to_owned(),
                    reason: reason.clone(),
                    blocking: !(skips && on_missing == OnMissing::Skip),
                });
                for desc in &component.engines {
                    let stub = if skips {
                        ComponentEngine::unavailable_declining(name, desc, &sha, reason.clone())
                    } else {
                        ComponentEngine::unavailable_failing(name, desc, &sha, reason.clone())
                    };
                    match stub {
                        Ok(engine) => built.push(engine),
                        Err(error) => report
                            .missing
                            .push(incompatible(name, format!("incompatible: {name}: {error:#}"))),
                    }
                }
            }
        }
        engines.insert(name, built);
    }
    if let Hosted::Host(host, true) = &hosted {
        host.seal();
    }

    let mut by_pair: FxHashMap<(usize, Language), Vec<ComponentEngine>> = FxHashMap::default();
    let per_pair = pairs.into_iter().map(|pair| (pair, false));
    let whole_tree_only = ancestor_pairs.into_iter().map(|pair| (pair, true));
    for ((config_id, language), whole_tree_only) in per_pair.chain(whole_tree_only) {
        let config = configs.config(config_id);
        let chosen = if whole_tree_only {
            selected_whole_tree_components(index, config, &language, &feature_is_linked, &selected)
        } else {
            selected_components(index, config, &language, &feature_is_linked, &selected)
        };
        let serving: Vec<ComponentEngine> = chosen
            .into_iter()
            .filter_map(|component| engines.get(component.name.as_str()))
            .flatten()
            .filter(|engine| engine.languages().is_empty() || engine.languages().contains(&language))
            .cloned()
            .collect();
        if !serving.is_empty() {
            by_pair.insert((config_id, language), serving);
        }
    }
    ResolvedComponents { by_pair, report }
}
