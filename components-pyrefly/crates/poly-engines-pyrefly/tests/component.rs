//! The real `pyrefly` component, built by alef from this workspace and loaded from a `file://` dev
//! lock, against the same engine linked in-process.
//!
//! Runs only when `POLY_COMPONENT_DEV_LOCK` names a lock written by
//! `mise run components:dev pyrefly`; otherwise it reports that it skipped and passes.

use poly_component_host::{ComponentHost, HostOptions, SlotStatus};
use poly_component_protocol::decode_describe;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, ProjectEnvironment, ProjectFile, ProjectRequest};
use poly_engine_api::language::Language;
use poly_engines::PolyComponent;
use poly_engines_pyrefly::{PyreflyComponent, PyreflyEngine};

const DEV_LOCK_ENV: &str = "POLY_COMPONENT_DEV_LOCK";

const KNOWN_BAD: &str = "count: int = \"three\"\n";

#[test]
fn the_pyrefly_component_checks_like_the_in_process_engine() {
    let Some(lock) = std::env::var(DEV_LOCK_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!("skipped: {DEV_LOCK_ENV} is not set; run `mise run components:dev pyrefly` first");
        return;
    };
    let lock = std::fs::read_to_string(&lock).unwrap_or_else(|e| panic!("reading {lock}: {e}"));
    let cache = tempfile::tempdir().unwrap();
    let host = ComponentHost::new(HostOptions::new(lock, cache.path())).expect("host over the dev lock");

    let describe =
        decode_describe(&PyreflyComponent::default().describe().unwrap()).expect("in-process describe decodes");
    let wire = describe
        .engines
        .first()
        .expect("the pyrefly component hosts an engine")
        .clone();
    assert_eq!(wire.name, "pyrefly");
    assert!(wire.capabilities.project);
    let engine = host
        .engine("pyrefly", &wire, Some(describe))
        .expect("engine proxy over the component");
    assert_eq!(engine.slot().status(), SlotStatus::Unloaded);

    let project = tempfile::tempdir().unwrap();
    let path = project.path().join("app.py");
    std::fs::write(&path, KNOWN_BAD).unwrap();
    let files = [ProjectFile {
        path: path.clone(),
        language: Language::Python,
        content: KNOWN_BAD.into(),
    }];
    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::from_str("backend = \"pyrefly\"").unwrap(),
    };
    let request = ProjectRequest {
        root: project.path(),
        files: &files,
        config: &cfg,
        environment: &ProjectEnvironment::default(),
        threads: Some(2),
    };
    let expected = PyreflyEngine.project_check(&request).expect("in-process check");
    let actual = engine.project_check(&request).expect("component check");
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    assert_eq!(actual[0].1[0].code.as_deref(), Some("bad-assignment"));
    assert_eq!(engine.slot().status(), SlotStatus::Ready);
}
