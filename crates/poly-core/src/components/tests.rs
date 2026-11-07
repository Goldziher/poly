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
        project_inputs: Default::default(),
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
    let selected = selected_components(&index, &Config::default(), &lua(), &linked, &|_| true);
    assert_eq!(names(selected), ["lua", "everywhere"]);
}

#[test]
fn only_a_language_without_a_component_gets_the_cross_cutting_one() {
    let index = index();
    let selected = selected_components(&index, &Config::default(), &Language::Rust, &linked, &|_| true);
    assert_eq!(names(selected), ["everywhere"]);
}

#[test]
fn an_opt_in_component_needs_its_engine_table_enabled() {
    let mut config = Config::default();
    config.engines.insert("speller".into(), EngineTable::default());
    assert_eq!(
        names(selected_components(&index(), &config, &lua(), &linked, &|_| true)),
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
        names(selected_components(&index(), &config, &lua(), &linked, &|_| true)),
        ["lua", "everywhere", "spell"]
    );
}

#[test]
fn an_opt_in_component_enabled_through_a_v1_lint_table_is_selected() {
    let per_language = Config {
        lint: toml::toml! { [lua.speller] enabled = true },
        ..Config::default()
    };
    assert_eq!(
        names(selected_components(&index(), &per_language, &lua(), &linked, &|_| true)),
        ["lua", "everywhere", "spell"],
        "[lint.<lang>.<engine>] enabled = true opts in"
    );

    // typos is cross-cutting, so v1 also reads its language-agnostic `[lint.typos]` table.
    let mut index = index();
    index
        .components
        .push(component("typos", "engine-typos", true, vec![engine("typos", &[])]));
    let cross_cutting = Config {
        lint: toml::toml! { [typos] enabled = true },
        ..Config::default()
    };
    assert_eq!(
        names(selected_components(&index, &cross_cutting, &lua(), &linked, &|_| true)),
        ["lua", "everywhere", "typos"],
        "[lint.typos] enabled = true opts in"
    );
}

fn index_with_uncomment() -> ComponentIndex {
    let mut index = index();
    let mut lint_only = engine("uncomment", &[]);
    lint_only.capabilities.format = false;
    index
        .components
        .push(component("uncomment", "engine-uncomment", true, vec![lint_only]));
    index
}

#[test]
fn the_uncomment_component_is_selected_only_when_enabled() {
    let index = index_with_uncomment();
    let select = |config: &Config| names(selected_components(&index, config, &lua(), &linked, &|_| true));
    let needed = |config: &Config| {
        let pairs = [(0, lua())];
        needed_components(&index, &[config], &pairs, &linked, &|_| true)
            .into_iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
    };

    let absent = Config::default();
    assert!(!select(&absent).contains(&"uncomment"), "no key, no component");
    assert!(!needed(&absent).contains(&"uncomment".to_owned()));

    let global = Config {
        lint: toml::toml! { [uncomment] enabled = true },
        ..Config::default()
    };
    assert!(
        select(&global).contains(&"uncomment"),
        "[lint.uncomment] enabled = true"
    );
    assert!(needed(&global).contains(&"uncomment".to_owned()));

    let per_language = Config {
        lint: toml::toml! { [lua.uncomment] enabled = true },
        ..Config::default()
    };
    assert!(select(&per_language).contains(&"uncomment"), "[lint.<lang>.uncomment]");
    let other_language = names(selected_components(
        &index,
        &per_language,
        &Language::Rust,
        &linked,
        &|_| true,
    ));
    assert!(!other_language.contains(&"uncomment"), "another language stays off");

    let mut engines = Config::default();
    engines.engines.insert(
        "uncomment".into(),
        EngineTable {
            enabled: Some(true),
            ..EngineTable::default()
        },
    );
    assert!(
        select(&engines).contains(&"uncomment"),
        "[engines.uncomment] enabled = true"
    );
    assert!(needed(&engines).contains(&"uncomment".to_owned()));
}

#[test]
fn an_opt_in_component_ignores_tables_for_a_capability_it_lacks() {
    let mut lint_only = engine("typos", &[]);
    lint_only.capabilities.format = false;
    let mut index = index();
    index
        .components
        .push(component("typos", "engine-typos", true, vec![lint_only]));
    let fmt_table = Config {
        fmt: toml::toml! { [typos] enabled = true },
        ..Config::default()
    };
    assert_eq!(
        names(selected_components(&index, &fmt_table, &lua(), &linked, &|_| true)),
        ["lua", "everywhere"],
        "a lint-only engine never runs from [fmt], so [fmt.typos] does not opt in"
    );
}

#[test]
fn an_explicit_list_is_exact_and_presets_expand() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["lua".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(
        names(selected_components(&index(), &config, &lua(), &linked, &|_| true)),
        ["lua"]
    );
    let preset = self::config(ComponentsConfig {
        enable: Enable::List(vec!["all".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(
        names(selected_components(
            &index(),
            &preset,
            &Language::Rust,
            &linked,
            &|_| true
        )),
        ["lua", "everywhere"]
    );
}

#[test]
fn disable_removes_a_component_or_a_preset() {
    let config = config(ComponentsConfig {
        disable: vec!["all".into()],
        ..ComponentsConfig::default()
    });
    assert!(selected_components(&index(), &config, &lua(), &linked, &|_| true).is_empty());
}

#[test]
fn front_ends_and_linked_components_are_never_needed() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["server".into(), "builtin".into()]),
        ..ComponentsConfig::default()
    });
    assert!(selected_components(&index(), &config, &lua(), &linked, &|_| true).is_empty());
    assert!(needed_components(&index(), &[&config], &[], &linked, &|_| true).is_empty());
}

#[test]
fn explicit_selections_are_needed_even_when_no_file_matches() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["lua".into()]),
        ..ComponentsConfig::default()
    });
    assert_eq!(
        names(needed_components(&index(), &[&config], &[], &linked, &|_| true)),
        ["lua"]
    );
    let auto = Config::default();
    assert!(needed_components(&index(), &[&auto], &[], &linked, &|_| true).is_empty());
}

#[test]
fn needed_is_the_union_over_pairs_in_index_order() {
    let auto = Config::default();
    let pairs = [(0, Language::Rust), (0, lua())];
    assert_eq!(
        names(needed_components(&index(), &[&auto], &pairs, &linked, &|_| true)),
        ["lua", "everywhere"]
    );
}

fn skipping<'a>(skipped: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |name| !skipped.contains(&name)
}

fn needed_for_lua(config: &Config, selected: &dyn Fn(&str) -> bool) -> Vec<String> {
    let pairs = [(0, lua())];
    needed_components(&index(), &[config], &pairs, &linked, selected)
        .into_iter()
        .map(|c| c.name.clone())
        .collect()
}

#[test]
fn a_component_whose_engines_are_all_skipped_is_not_needed() {
    let config = Config::default();
    let skipped = ["lualint"];
    assert_eq!(needed_for_lua(&config, &skipping(&skipped)), ["everywhere"]);
    let skipped = ["lualint", "scanner"];
    assert!(needed_for_lua(&config, &skipping(&skipped)).is_empty());
}

#[test]
fn only_naming_none_of_a_components_engines_drops_it() {
    let config = Config::default();
    let only = |name: &str| name == "scanner";
    assert_eq!(needed_for_lua(&config, &only), ["everywhere"]);
    let only = |name: &str| name == "native";
    assert!(needed_for_lua(&config, &only).is_empty());
}

#[test]
fn a_component_with_one_engine_still_selected_stays_needed() {
    let mut idx = index();
    idx.components[0].engines.push(engine("lualint2", &["lua"]));
    let skipped = ["lualint"];
    let selected = skipping(&skipped);
    let config = Config::default();
    let pairs = [(0, lua())];
    assert_eq!(
        names(needed_components(&idx, &[&config], &pairs, &linked, &selected)),
        ["lua", "everywhere"]
    );
}

#[test]
fn a_skipped_engine_serves_no_other_language_either() {
    let config = Config::default();
    let skipped = ["scanner"];
    let selected = skipping(&skipped);
    assert!(selected_components(&index(), &config, &Language::Rust, &linked, &selected).is_empty());
}

#[test]
fn skipping_the_only_engine_for_a_language_drops_a_component_whose_other_engines_serve_others() {
    let mut idx = index();
    idx.components[0].engines.push(engine("luafmt", &["luau"]));
    let skipped = ["lualint"];
    let selected = skipping(&skipped);
    let config = Config::default();
    let pairs = [(0, lua())];
    assert_eq!(
        names(needed_components(&idx, &[&config], &pairs, &linked, &selected)),
        ["everywhere"],
        "luafmt serves luau, not lua"
    );
}

#[test]
fn a_v2_engine_table_with_enabled_false_drops_the_component() {
    let mut config = Config::default();
    config.engines.insert(
        "lualint".into(),
        EngineTable {
            enabled: Some(false),
            ..EngineTable::default()
        },
    );
    assert_eq!(needed_for_lua(&config, &|_| true), ["everywhere"]);
}

#[test]
fn v1_enabled_false_in_both_phases_drops_the_component_but_one_phase_does_not() {
    let mut config = Config {
        lint: toml::from_str("[lua.lualint]\nenabled = false\n").unwrap(),
        ..Config::default()
    };
    assert_eq!(
        needed_for_lua(&config, &|_| true),
        ["lua", "everywhere"],
        "the format phase may still run it"
    );
    config.fmt = toml::from_str("[lua.lualint]\nenabled = false\n").unwrap();
    assert_eq!(needed_for_lua(&config, &|_| true), ["everywhere"]);
}

#[test]
fn an_explicit_selection_is_not_needed_when_its_engines_are_skipped() {
    let config = config(ComponentsConfig {
        enable: Enable::List(vec!["lua".into()]),
        ..ComponentsConfig::default()
    });
    let skipped = ["lualint"];
    assert!(needed_components(&index(), &[&config], &[], &linked, &skipping(&skipped)).is_empty());
    assert_eq!(
        names(needed_components(&index(), &[&config], &[], &linked, &|_| true)),
        ["lua"]
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
            &super::feature_is_linked,
            &|_| true
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
