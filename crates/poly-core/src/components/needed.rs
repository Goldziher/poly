//! Which components a run needs. Pure: no host, no network, no I/O.

use std::collections::BTreeSet;

use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_config::v2::Enable;

use crate::config::Config;
use crate::language::Language;

/// The `poly-core` engine features compiled into this binary.
const LINKED_FEATURES: &[&str] = &[
    #[cfg(feature = "engine-web")]
    "engine-web",
    #[cfg(feature = "engine-python")]
    "engine-python",
    #[cfg(feature = "engine-php")]
    "engine-php",
    #[cfg(feature = "engine-sql")]
    "engine-sql",
    #[cfg(feature = "engine-biome")]
    "engine-biome",
    #[cfg(feature = "engine-ruby")]
    "engine-ruby",
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

/// Whether the `poly-core` cargo feature `name` is compiled into this binary. An unknown name is
/// not linked, so a component naming it is treated as downloadable.
#[must_use]
pub(crate) fn feature_is_linked(name: &str) -> bool {
    LINKED_FEATURES.contains(&name)
}

/// Whether every engine family `component` is built from is linked into this binary, so it
/// never needs downloading.
fn is_linked(component: &Component, linked: &dyn Fn(&str) -> bool) -> bool {
    !component.features.is_empty() && component.features.iter().all(|f| linked(f))
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
/// the cross-cutting ones), minus opt-in components; an explicit list is exact. Explicit
/// `[engines.X]` tables add to either. `disable` removes, and components whose families are
/// linked ([`feature_is_linked`]) and front ends never appear.
#[must_use]
pub(crate) fn selected_components<'a>(
    index: &'a ComponentIndex,
    config: &Config,
    language: &Language,
    linked: &dyn Fn(&str) -> bool,
) -> Vec<&'a Component> {
    let mut names = explicit(index, config);
    if config.components.enable == Enable::Auto {
        names.extend(
            index
                .components_for_language(language.id())
                .into_iter()
                .chain(index.cross_cutting_components())
                .filter(|c| !c.opt_in)
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
) -> Vec<&'a Component> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for (config_id, language) in pairs {
        names.extend(
            selected_components(index, configs[*config_id], language, linked)
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
                    index
                        .component(n)
                        .is_some_and(|c| c.kind == ComponentKind::Engines && !is_linked(c, linked))
                }),
        );
    }
    index
        .components
        .iter()
        .filter(|c| names.contains(c.name.as_str()))
        .collect()
}
