//! The real `web` component, built by alef from this workspace and loaded from a `file://` dev lock,
//! against the same engine linked in-process.
//!
//! Runs only when `POLY_COMPONENT_DEV_LOCK` names a lock written by
//! `mise run components:dev web`; otherwise it reports that it skipped and passes.

use std::path::PathBuf;
use std::sync::Arc;

use poly_component_host::{ComponentHost, HostOptions, SlotStatus};
use poly_component_protocol::decode_describe;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Engine, FormatOutput, SourceFile};
use poly_engine_api::language::Language;
use poly_engines::PolyComponent;
use poly_engines_web::{OxcEngine, WebComponent};

const DEV_LOCK_ENV: &str = "POLY_COMPONENT_DEV_LOCK";

/// Irregular spacing, single quotes and a missing semicolon, all of which oxc normalizes.
const UNFORMATTED: &str = "const   a  =  'x'\n";

fn formatted(output: FormatOutput) -> String {
    match output {
        FormatOutput::Formatted(text) => text,
        FormatOutput::Unchanged => panic!("the fixture must be reformatted"),
    }
}

#[test]
fn the_web_component_describes_every_web_engine() {
    let describe = decode_describe(&WebComponent::default().describe().unwrap()).expect("in-process describe decodes");
    let mut names: Vec<_> = describe.engines.iter().map(|engine| engine.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["adrlint", "malva", "markup_fmt", "oxc", "rumdl", "taplo", "yaml"]
    );
}

#[test]
fn the_web_component_formats_like_the_in_process_engine() {
    let Some(lock) = std::env::var(DEV_LOCK_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!("skipped: {DEV_LOCK_ENV} is not set; run `mise run components:dev web` first");
        return;
    };
    let lock = std::fs::read_to_string(&lock).unwrap_or_else(|e| panic!("reading {lock}: {e}"));
    let cache = tempfile::tempdir().unwrap();
    let host = ComponentHost::new(HostOptions::new(lock, cache.path())).expect("host over the dev lock");

    let describe = decode_describe(&WebComponent::default().describe().unwrap()).expect("in-process describe decodes");
    let wire = describe
        .engines
        .iter()
        .find(|engine| engine.name == "oxc")
        .expect("the web component hosts the oxc engine")
        .clone();
    let engine = host
        .engine("web", &wire, Some(describe))
        .expect("engine proxy over the component");
    assert_eq!(engine.slot().status(), SlotStatus::Unloaded);

    let cfg = EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: Language::JavaScript.default_indent_width(),
        options: toml::Table::new(),
    };
    let src = SourceFile {
        path: PathBuf::from("test.js"),
        language: Language::JavaScript,
        content: Arc::from(UNFORMATTED),
    };
    let expected = formatted(OxcEngine.format(&src, &cfg).expect("in-process format"));
    let actual = formatted(engine.format(&src, &cfg).expect("component format"));
    assert_eq!(actual, expected);
    assert_eq!(engine.slot().status(), SlotStatus::Ready);
}
