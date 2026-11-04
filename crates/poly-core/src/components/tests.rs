use std::collections::BTreeMap;

use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_component_protocol::{WireCapabilities, WireEngineDesc};
use poly_config::v2::{ComponentsConfig, Enable, EngineTable};

use super::{ComponentsReport, MissingComponent, needed_components, selected_components};
use crate::config::Config;
use crate::language::Language;

fn engine(name: &str, languages: &[&str]) -> WireEngineDesc {
    WireEngineDesc {
        name: name.into(),
        languages: languages.iter().map(|l| (*l).to_owned()).collect(),
        capabilities: WireCapabilities {
            lint: true,
            format: true,
            fix: false,
            project: false,
        },
        version: "1.0.0".into(),
        option_keys: Vec::new(),
        hooks: 0,
        takes_over_tier_one: false,
        claimed_paths: Vec::new(),
        host_hook: "none".into(),
        supplements_tier_one: false,
        self_manages_enabled: false,
        supersedes_generic_formatter: false,
        batch_shard_floor: 2,
    }
}

fn component(name: &str, feature: &str, opt_in: bool, engines: Vec<WireEngineDesc>) -> Component {
    Component {
        name: name.into(),
        kind: ComponentKind::Engines,
        opt_in,
        features: vec![feature.into()],
        engines,
        lock_sha: None,
    }
}

fn index() -> ComponentIndex {
    ComponentIndex {
        schema: 1,
        components: vec![
            component("lua", "engine-lua", false, vec![engine("lualint", &["lua"])]),
            component("everywhere", "engine-everywhere", false, vec![engine("scanner", &[])]),
            component("spell", "engine-spell", true, vec![engine("speller", &[])]),
            component("builtin", "engine-builtin", false, vec![engine("native", &["lua"])]),
            Component {
                name: "server".into(),
                kind: ComponentKind::Frontend,
                ..Component::default()
            },
        ],
        presets: BTreeMap::from([("all".to_owned(), vec!["lua".to_owned(), "everywhere".to_owned()])]),
    }
}

fn linked(feature: &str) -> bool {
    feature == "engine-builtin"
}

fn names(components: Vec<&Component>) -> Vec<&str> {
    components.into_iter().map(|c| c.name.as_str()).collect()
}

fn config(components: ComponentsConfig) -> Config {
    Config {
        components,
        ..Config::default()
    }
}

fn lua() -> Language {
    Language::Other("lua".into())
}

#[test]
fn auto_derives_the_language_and_cross_cutting_components_but_not_opt_in_or_linked_ones() {
    let index = index();
    let selected = selected_components(&index, &Config::default(), &lua(), &linked);
    assert_eq!(names(selected), ["lua", "everywhere"]);
}

#[test]
fn only_a_language_without_a_component_gets_the_cross_cutting_one() {
    let index = index();
    let selected = selected_components(&index, &Config::default(), &Language::Rust, &linked);
    assert_eq!(names(selected), ["everywhere"]);
}

#[test]
fn an_opt_in_component_needs_its_engine_table_enabled() {
    let mut config = Config::default();
    config.engines.insert("speller".into(), EngineTable::default());
    assert_eq!(
        names(selected_components(&index(), &config, &lua(), &linked)),
        ["lua", "everywhere"],
        "a bare table does not opt in"
    );
    config.engines.insert(
        "speller".into(),
        EngineTable {
            enabled: Some(true),
            ..EngineTable::default()
        },
    );
    assert_eq!(
        names(selected_components(&index(), &config, &lua(), &linked)),
        ["lua", "everywhere", "spell"]
    );
}

#[test]
fn an_explicit_list_is_exact_and_presets_expand() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["lua".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(names(selected_components(&index(), &config, &lua(), &linked)), ["lua"]);
    let preset = self::config(ComponentsConfig {
        enable: Enable::List(vec!["all".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(
        names(selected_components(&index(), &preset, &Language::Rust, &linked)),
        ["lua", "everywhere"]
    );
}

#[test]
fn disable_removes_a_component_or_a_preset() {
    let config = config(ComponentsConfig {
        disable: vec!["all".into()],
        ..ComponentsConfig::default()
    });
    assert!(selected_components(&index(), &config, &lua(), &linked).is_empty());
}

#[test]
fn front_ends_and_linked_components_are_never_needed() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["server".into(), "builtin".into()]),
        ..ComponentsConfig::default()
    });
    assert!(selected_components(&index(), &config, &lua(), &linked).is_empty());
    assert!(needed_components(&index(), &[&config], &[], &linked).is_empty());
}

#[test]
fn explicit_selections_are_needed_even_when_no_file_matches() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["lua".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(names(needed_components(&index(), &[&config], &[], &linked)), ["lua"]);
    let auto = Config::default();
    assert!(needed_components(&index(), &[&auto], &[], &linked).is_empty());
}

#[test]
fn needed_is_the_union_over_pairs_in_index_order() {
    let auto = Config::default();
    let pairs = [(0, Language::Rust), (0, lua())];
    assert_eq!(
        names(needed_components(&index(), &[&auto], &pairs, &linked)),
        ["lua", "everywhere"]
    );
}

/// Families whose engines live outside `poly-core`: a build links them only through an engine
/// provider that declares the feature with `register_linked_features`.
#[cfg(feature = "all-engines")]
const EXTERNAL_FAMILIES: &[&str] = &["engine-biome", "engine-ruby", "engine-web"];

#[cfg(feature = "all-engines")]
#[test]
fn the_committed_index_needs_only_the_external_families_in_the_core_build() {
    let committed = poly_component_index::index();
    for component in committed.components.iter().filter(|c| c.kind == ComponentKind::Engines) {
        for feature in &component.features {
            assert!(
                super::feature_is_linked(feature) || EXTERNAL_FAMILIES.contains(&feature.as_str()),
                "{feature} is not recognised"
            );
        }
    }
    let pairs: Vec<(usize, Language)> = crate::registry::all_languages().into_iter().map(|l| (0, l)).collect();
    let config = Config::default();
    assert_eq!(
        names(needed_components(
            committed,
            &[&config],
            &pairs,
            &super::feature_is_linked
        )),
        ["web", "biome", "ruby"]
    );
}

#[test]
fn the_report_is_empty_until_a_component_is_needed_and_blocking_follows_its_entries() {
    let mut report = ComponentsReport::default();
    assert!(report.is_empty());
    assert!(!report.blocking());
    report.missing.push(MissingComponent {
        name: "lua".into(),
        kind: "offline".into(),
        reason: "component not installed: lua (offline)".into(),
        blocking: false,
    });
    assert!(!report.is_empty());
    assert!(!report.blocking());
    report.missing[0].blocking = true;
    assert!(report.blocking());
}
