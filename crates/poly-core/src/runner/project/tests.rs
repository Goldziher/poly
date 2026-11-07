use super::*;
use crate::config::Config;
use poly_component_host::ComponentEngine;
use poly_component_protocol::{WireCapabilities, WireEngineDesc};

/// The `pyrefly` engine as the component index describes it: a Python project check.
fn pyrefly_component() -> ComponentEngine {
    ComponentEngine::unavailable_declining("pyrefly", &pyrefly_desc(), "0000000000000000", "test").unwrap()
}

fn pyrefly_desc() -> WireEngineDesc {
    WireEngineDesc {
        name: "pyrefly".into(),
        languages: vec!["python".into()],
        capabilities: WireCapabilities {
            lint: false,
            format: false,
            fix: false,
            project: true,
        },
        version: "pyrefly-test".into(),
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
        selection: Some(WireSelection {
            table: "python-types".into(),
            key: "backend".into(),
            value: "pyrefly".into(),
            default: false,
        }),
    }
}

fn planned(config: Config) -> Vec<&'static str> {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let configs = ConfigSet::single(config);
    let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
    plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&[], &[]),
        &components,
        &[],
        poly_component_index::index(),
    )
    .0
    .iter()
    .map(|plan| plan.engine.name())
    .collect()
}

fn with_backend(backend: &str) -> Config {
    let lint: toml::Table = toml::from_str(&format!("[python.python-types]\nbackend = \"{backend}\"\n")).unwrap();
    Config {
        lint,
        ..Config::default()
    }
}

#[test]
fn python_types_is_not_planned_without_a_backend() {
    assert!(planned(Config::default()).is_empty());
}

#[test]
fn the_ty_backend_plans_ty_and_not_pyrefly() {
    assert_eq!(planned(with_backend("ty")), ["python-types"]);
}

#[test]
fn the_pyrefly_backend_plans_the_component_engine_and_ty_stands_down() {
    assert_eq!(planned(with_backend("pyrefly")), ["pyrefly"]);
}

#[test]
fn an_unknown_backend_plans_the_default_so_it_can_report_the_value() {
    assert_eq!(planned(with_backend("nonsense")), ["python-types"]);
}

#[test]
fn the_routed_engine_reads_the_shared_table() {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let lint: toml::Table =
        toml::from_str("[python.python-types]\nbackend = \"pyrefly\"\npython_version = \"3.11\"\n").unwrap();
    let configs = ConfigSet::single(Config {
        lint,
        ..Config::default()
    });
    let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
    let (plans, _, _) = plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&[], &[]),
        &components,
        &[],
        poly_component_index::index(),
    );
    assert_eq!(plans.len(), 1);
    assert_eq!(
        plans[0]
            .config
            .options
            .get("python_version")
            .and_then(toml::Value::as_str),
        Some("3.11")
    );
}

#[test]
fn switching_the_shared_table_off_stops_both_backends() {
    let lint: toml::Table = toml::from_str("[python.python-types]\nbackend = \"pyrefly\"\nenabled = false\n").unwrap();
    let config = Config {
        lint,
        ..Config::default()
    };
    assert!(planned(config).is_empty());
}

fn planned_with(config: Config, only: &[&str], skip: &[&str]) -> Vec<&'static str> {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let (only, skip): (Vec<String>, Vec<String>) = (
        only.iter().map(|s| (*s).to_owned()).collect(),
        skip.iter().map(|s| (*s).to_owned()).collect(),
    );
    let configs = ConfigSet::single(config);
    let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
    plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&only, &skip),
        &components,
        &[],
        poly_component_index::index(),
    )
    .0
    .iter()
    .map(|plan| plan.engine.name())
    .collect()
}

#[test]
fn skipping_the_shared_table_stops_the_pyrefly_backend() {
    assert!(planned_with(with_backend("pyrefly"), &[], &["python-types"]).is_empty());
    assert_eq!(planned_with(with_backend("pyrefly"), &[], &["ruff"]), ["pyrefly"]);
}

#[test]
fn only_the_shared_table_keeps_the_pyrefly_backend() {
    assert_eq!(
        planned_with(with_backend("pyrefly"), &["python-types"], &[]),
        ["pyrefly"]
    );
    assert_eq!(planned_with(with_backend("pyrefly"), &["pyrefly"], &[]), ["pyrefly"]);
    assert!(planned_with(with_backend("pyrefly"), &["ruff"], &[]).is_empty());
}

#[test]
fn the_table_name_selects_the_ty_backend_too() {
    assert_eq!(
        planned_with(with_backend("ty"), &["python-types"], &[]),
        ["python-types"]
    );
    assert!(planned_with(with_backend("ty"), &[], &["python-types"]).is_empty());
}

#[test]
fn a_non_string_backend_plans_the_default_so_it_can_report_the_value() {
    for value in ["1", "true", "[\"pyrefly\"]"] {
        let lint: toml::Table = toml::from_str(&format!("[python.python-types]\nbackend = {value}\n")).unwrap();
        let config = Config {
            lint,
            ..Config::default()
        };
        assert_eq!(planned(config), ["python-types"], "backend = {value}");
    }
}

#[test]
fn a_backend_whose_engine_this_run_lacks_is_an_error_naming_the_component() {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let configs = ConfigSet::single(with_backend("pyrefly"));
    let no_components = ResolvedComponents::default();
    let (plans, errors, _) = plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&[], &[]),
        &no_components,
        &[],
        poly_component_index::index(),
    );
    assert!(plans.is_empty());
    assert_eq!(
        errors.len(),
        1,
        "{:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    let message = &errors[0].message;
    assert!(
        message.contains("`pyrefly` component") && message.contains("[components] disable"),
        "{message}"
    );

    let skip = ["pyrefly".to_owned()];
    let (_, errors, _) = plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&[], &skip),
        &no_components,
        &[],
        poly_component_index::index(),
    );
    assert!(errors.is_empty(), "a run that skipped the backend asked for that");
}

fn planned_all(config: Config, only: &[&str], index: &ComponentIndex) -> (usize, Vec<String>, Vec<String>) {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let only: Vec<String> = only.iter().map(|s| (*s).to_owned()).collect();
    let configs = ConfigSet::single(config);
    let components = ResolvedComponents::serving(0, Language::Python, vec![pyrefly_component()]);
    let (plans, errors, skips) = plan_project_engines(
        &files,
        &configs,
        EngineSelection::new(&only, &[]),
        &components,
        &[],
        index,
    );
    (
        plans.len(),
        errors.into_iter().map(|e| e.message).collect(),
        skips.into_iter().map(|s| s.reason).collect(),
    )
}

#[test]
fn only_pyrefly_over_another_backend_is_a_counted_skip_naming_why() {
    let (plans, errors, skips) = planned_all(with_backend("ty"), &["pyrefly"], poly_component_index::index());
    assert_eq!((plans, errors.len()), (0, 0));
    assert_eq!(
        skips,
        ["`--only pyrefly` selects nothing here: `[python-types] backend` is \"ty\""]
    );
    assert!(
        !crate::is_withdrawal_reason(&skips[0]),
        "must be charged to --deny-skips"
    );
    let (_, _, skips) = planned_all(Config::default(), &["python-types"], poly_component_index::index());
    assert_eq!(
        skips,
        ["`--only python-types` selects nothing here: `[python-types] backend` is unset"]
    );
}

#[test]
fn an_unrelated_only_or_a_satisfied_one_adds_no_skip() {
    let index = poly_component_index::index();
    assert!(planned_all(with_backend("ty"), &["ruff"], index).2.is_empty());
    assert!(planned_all(with_backend("pyrefly"), &["pyrefly"], index).2.is_empty());
    assert!(planned_all(with_backend("ty"), &[], index).2.is_empty());
}

fn fake_index() -> ComponentIndex {
    let mut desc = pyrefly_desc();
    desc.name = "fake".into();
    desc.selection = Some(WireSelection {
        table: "fake-types".into(),
        key: "backend".into(),
        value: "x".into(),
        default: true,
    });
    ComponentIndex {
        components: vec![poly_component_index::Component {
            name: "fakes".into(),
            engines: vec![desc],
            ..Default::default()
        }],
        ..ComponentIndex::default()
    }
}

#[test]
fn a_missing_default_engine_is_an_error_not_silence() {
    let lint: toml::Table = toml::from_str("[python.fake-types]\nbackend = \"x\"\n").unwrap();
    let config = Config {
        lint,
        ..Config::default()
    };
    let (_, errors, _) = planned_all(config, &[], &fake_index());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].contains("`fakes` component"), "{}", errors[0]);
}

#[test]
fn the_run_index_decides_what_is_claimed() {
    let files = [DiscoveredFile {
        path: PathBuf::from("app.py"),
        language: Language::Python,
        config_id: 0,
    }];
    let configs = ConfigSet::single(with_backend("pyrefly"));
    let none = ResolvedComponents::default();
    let empty = ComponentIndex::default();
    let (plans, errors, _) = plan_project_engines(&files, &configs, EngineSelection::new(&[], &[]), &none, &[], &empty);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        plans.len(),
        1,
        "no engine of this run's index claims `pyrefly`, so the default reports it"
    );
}
