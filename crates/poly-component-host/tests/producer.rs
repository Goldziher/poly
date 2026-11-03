//! The host against the real producer adapter, so the two halves of the protocol agree on wording
//! and framing rather than each matching its own fake.

use std::path::PathBuf;
use std::sync::Arc;

use poly_component_host::{ComponentEngine, ComponentSlot, ComponentTransport, InitParams};
use poly_component_protocol::decode_describe;
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::{Capabilities, Diagnostic, Engine, Severity, SourceFile};
use poly_engine_api::language::Language;
use poly_engines::PolyComponent;
use poly_engines::producer::Producer;

struct Probe;

impl Engine for Probe {
    fn name(&self) -> &'static str {
        "probe"
    }
    fn languages(&self) -> &'static [Language] {
        &[Language::Sql]
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: false,
            project: false,
        }
    }
    fn version(&self) -> &str {
        "1.0"
    }
    fn lint(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match &*src.content {
            "panic" => panic!("index out of bounds"),
            "error" => Err(anyhow::anyhow!("bad token").context("parse failed")),
            text => Ok(vec![Diagnostic {
                engine: "probe".into(),
                code: Some("P001".into()),
                severity: Severity::Warning,
                title: format!("{} bytes", text.len()),
                description: None,
                span: None,
                url: None,
                fix: Vec::new(),
                metadata: Default::default(),
            }]),
        }
    }
}

struct InProcess(Producer);

impl ComponentTransport for InProcess {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.describe()?)
    }
    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.call(header, body)?)
    }
}

fn engine() -> ComponentEngine {
    let transport = Arc::new(InProcess(Producer::new("1.0.0", vec![Box::new(Probe)])));
    let describe = decode_describe(&transport.describe().unwrap()).unwrap();
    let desc = describe.engines[0].clone();
    let slot = ComponentSlot::from_transport("probe-component", Some(describe), InitParams::default(), transport);
    ComponentEngine::new("probe-component", &desc, "0123456789abcdef0123456789abcdef", slot).unwrap()
}

fn source(text: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from("q.sql"),
        language: Language::Sql,
        content: Arc::from(text),
    }
}

fn config() -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: 4,
        options: toml::Table::new(),
    }
}

#[test]
fn lint_round_trips_through_the_producer() {
    let found = engine().lint(&source("select 1"), &config()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code.as_deref(), Some("P001"));
    assert_eq!(found[0].title, "8 bytes");
}

#[test]
fn producer_panic_reads_exactly_like_the_engine_guard() {
    let error = engine().lint(&source("panic"), &config()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "the 'probe' backend panicked on this file: index out of bounds"
    );
}

#[test]
fn producer_error_keeps_its_chain() {
    let error = engine().lint(&source("error"), &config()).unwrap_err();
    let chain: Vec<String> = error.chain().map(ToString::to_string).collect();
    assert_eq!(chain, ["parse failed", "bad token"]);
}
