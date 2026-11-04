//! Which components a run needs. Pure: no host, no network, no I/O.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_component_protocol::WireEngineDesc;
use poly_config::v2::Enable;

use crate::config::{Config, Kind};
use crate::language::Language;

/// The `poly-core` engine features compiled into this binary.
const LINKED_FEATURES: &[&str] = &[
    #[cfg(feature = "engine-python")]
    "engine-python",
    #[cfg(feature = "engine-php")]
    "engine-php",
    #[cfg(feature = "engine-sql")]
    "engine-sql",
    #[cfg(feature = "engine-nix")]
    "engine-nix",
    #[cfg(feature = "engine-typos")]
    "engine-typos",
    #[cfg(feature = "engine-quality")]
    "engine-quality",
    #[cfg(feature = "engine-astgrep")]
    "engine-astgrep",
    #[cfg(feature = "engine-uncomment")]
    "engine-uncomment",
    #[cfg(feature = "engine-dockerfile")]
    "engine-dockerfile",
    #[cfg(feature = "engine-dotenv")]
    "engine-dotenv",
    #[cfg(feature = "engine-ini")]
    "engine-ini",
    #[cfg(feature = "engine-hcl")]
    "engine-hcl",
    #[cfg(feature = "engine-graphql")]
    "engine-graphql",
];

/// Component feature names a bundled binary links through an engine provider rather than a
/// `poly-core` cargo feature.
static PROVIDED_FEATURES: OnceLock<&'static [&'static str]> = OnceLock::new();

/// Declares the component feature names (`engine-ruby`, ...) whose engines the process supplies
/// through [`register_engine_provider`](crate::register_engine_provider), so the component planner
/// treats those components as linked and never downloads them.
///
/// Call it once at startup next to the provider registration. Returns `false` when a set was
/// already declared (the first wins).
pub fn register_linked_features(features: &'static [&'static str]) -> bool {
    PROVIDED_FEATURES.set(features).is_ok()
}

/// Whether the engine family named `name` is linked into this binary: a `poly-core` cargo feature
/// compiled in, or one declared through [`register_linked_features`]. An unknown name is not
/// linked, so a component naming it is treated as downloadable.
#[must_use]
pub(crate) fn feature_is_linked(name: &str) -> bool {
    LINKED_FEATURES.contains(&name) || PROVIDED_FEATURES.get().is_some_and(|provided| provided.contains(&name))
}

/// Whether every engine family `component` is built from is linked into this binary, so it
/// never needs downloading.
pub(super) fn is_linked(component: &Component, linked: &dyn Fn(&str) -> bool) -> bool {
    !component.features.is_empty() && component.features.iter().all(|f| linked(f))
}

/// Whether the engine named `name` survives the run's `--only` / `--skip` selection.
pub(crate) type Selected<'a> = &'a dyn Fn(&str) -> bool;

/// Whether `desc` can still run under `config`: the run's selection keeps it and no
/// `enabled = false` switches it off. With a `language`, v1 per-language tables and v2 language
/// layers count too, for lint and format alike (an engine off for only one phase can still run).
/// Without one, only the language-independent `[engines.<id>] enabled = false` does.
fn engine_in_play(config: &Config, language: Option<&Language>, desc: &WireEngineDesc, selected: Selected<'_>) -> bool {
    if !selected(&desc.name) {
        return false;
    }
    let off = |kind| {
        language.map_or_else(
            || config.engines.get(&desc.name).and_then(|table| table.enabled) == Some(false),
            |language| {
                config
                    .engine_config(language, &desc.name, kind)
                    .options
                    .get("enabled")
                    .and_then(toml::Value::as_bool)
                    == Some(false)
            },
        )
    };
    !(off(Kind::Lint) && off(Kind::Format))
}

/// Whether `config` switches on one of opt-in `component`'s engines for `language`, through any
/// table the engine's own `enabled` resolves from (a v1 `[lint.<lang>.<engine>]` or cross-cutting
/// `[lint.<engine>]` table as much as a v2 language layer).
fn opted_in(config: &Config, language: &Language, component: &Component) -> bool {
    component.engines.iter().any(|desc| {
        [Kind::Lint, Kind::Format].into_iter().any(|kind| {
            config
                .engine_config(language, &desc.name, kind)
                .options
                .get("enabled")
                .and_then(toml::Value::as_bool)
                == Some(true)
        })
    })
}

fn expand<'a>(index: &'a ComponentIndex, names: &[String]) -> impl Iterator<Item = &'a Component> {
    names.iter().filter_map(|name| index.expand_preset(name)).flatten()
}

/// Components `config` names outright, whatever the language: the `[components] enable` list
/// and the components of engines with an explicit `[engines.<id>]` table. An opt-in component
/// needs `enabled = true`; any other needs the table not to say `false`.
fn explicit<'a>(index: &'a ComponentIndex, config: &Config) -> BTreeSet<&'a str> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    if let Enable::List(list) = &config.components.enable {
        names.extend(expand(index, list).map(|c| c.name.as_str()));
    }
    for (engine, table) in &config.engines {
        let Some(component) = index.component_for_engine(engine) else {
            continue;
        };
        let wanted = if component.opt_in {
            table.enabled == Some(true)
        } else {
            table.enabled != Some(false)
        };
        if wanted {
            names.insert(component.name.as_str());
        }
    }
    names
}

/// The downloadable components that apply to `language` under `config`, in index order.
///
/// `enable = "auto"` derives them from the components hosting an engine for the language (and
/// the cross-cutting ones), minus opt-in components nothing switches on; an explicit list is exact. Explicit
/// `[engines.X]` tables add to either. `disable` removes, and components whose families are
/// linked ([`feature_is_linked`]) and front ends never appear. A component none of whose engines
/// is still in play (`selected`, `enabled = false`) is not selected, and a component derived from the language needs one of them to serve it: a run that
/// excludes all of them must not download it.
#[must_use]
pub(crate) fn selected_components<'a>(
    index: &'a ComponentIndex,
    config: &Config,
    language: &Language,
    linked: &dyn Fn(&str) -> bool,
    selected: Selected<'_>,
) -> Vec<&'a Component> {
    let named = explicit(index, config);
    let mut names = named.clone();
    if config.components.enable == Enable::Auto {
        names.extend(
            index
                .components_for_language(language.id())
                .into_iter()
                .chain(index.cross_cutting_components())
                .filter(|c| !c.opt_in || opted_in(config, language, c))
                .map(|c| c.name.as_str()),
        );
    }
    let disabled: BTreeSet<&str> = expand(index, &config.components.disable)
        .map(|c| c.name.as_str())
        .collect();
    index
        .components
        .iter()
        .filter(|c| names.contains(c.name.as_str()) && !disabled.contains(c.name.as_str()))
        .filter(|c| c.kind == ComponentKind::Engines && !is_linked(c, linked))
        .filter(|c| {
            // A component named outright serves whatever language its engines are asked for; one
            // derived from the language needs an engine for that language.
            let serves = |desc: &WireEngineDesc| {
                named.contains(c.name.as_str())
                    || desc.languages.is_empty()
                    || desc.languages.iter().any(|l| l == language.id())
            };
            c.engines
                .iter()
                .any(|desc| serves(desc) && engine_in_play(config, Some(language), desc, selected))
        })
        .collect()
}

/// Every downloadable component a run over `pairs` needs: those that apply to some pair, plus
/// each config's explicit selections even when no file of theirs was discovered. Sorted by
/// index order.
#[must_use]
pub(crate) fn needed_components<'a>(
    index: &'a ComponentIndex,
    configs: &[&Config],
    pairs: &[(usize, Language)],
    linked: &dyn Fn(&str) -> bool,
    selected: Selected<'_>,
) -> Vec<&'a Component> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for (config_id, language) in pairs {
        names.extend(
            selected_components(index, configs[*config_id], language, linked, selected)
                .into_iter()
                .map(|c| c.name.as_str()),
        );
    }
    for config in configs {
        let disabled: BTreeSet<&str> = expand(index, &config.components.disable)
            .map(|c| c.name.as_str())
            .collect();
        names.extend(
            explicit(index, config)
                .into_iter()
                .filter(|n| !disabled.contains(n))
                .filter(|n| {
                    index.component(n).is_some_and(|c| {
                        c.kind == ComponentKind::Engines
                            && !is_linked(c, linked)
                            && c.engines
                                .iter()
                                .any(|desc| engine_in_play(config, None, desc, selected))
                    })
                }),
        );
    }
    index
        .components
        .iter()
        .filter(|c| names.contains(c.name.as_str()))
        .collect()
}
