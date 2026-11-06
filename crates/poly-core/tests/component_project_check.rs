//! A project check served by a downloadable component runs in the project phase.
//!
//! The component is a fake `checker` engine for Python with the `project` capability, behind an injected
//! host and index, so the run is a core build serving the engine from a component rather than linking it.
#![cfg(feature = "all-engines")]

use std::path::PathBuf;
use std::sync::Arc;

use alef_component_runtime::ComponentManager;
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_component_protocol::{
    Describe, PROTOCOL_VERSION, Payload, Request, Response, WireCapabilities, WireDiagnostic, WireEngineDesc,
    WireFileDiagnostics, WireSeverity, decode_request, encode_describe, encode_response,
};
use poly_core::components::ComponentInjection;
use poly_core::{Config, RunOptions};

const TARGET: &str = "test-target";

fn desc() -> WireEngineDesc {
    WireEngineDesc {
        name: "checker".into(),
        languages: vec!["python".into()],
        capabilities: WireCapabilities {
            lint: false,
            format: false,
            fix: false,
            project: true,
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

fn index() -> ComponentIndex {
    ComponentIndex {
        schema: 1,
        components: vec![Component {
            name: "checker".into(),
            kind: ComponentKind::Engines,
            opt_in: false,
            features: vec!["engine-checker".into()],
            engines: vec![desc()],
            lock_sha: None,
        }],
        presets: Default::default(),
    }
}

fn lock() -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"checker","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://example.invalid/checker.tar.gz",
            "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32),
        sha = "ab".repeat(32)
    )
}

/// Answers every project check with one finding on the first file it was given.
struct Checker;

impl ComponentTransport for Checker {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(encode_describe(&Describe {
            protocol: PROTOCOL_VERSION,
            component_version: "1.0.0".into(),
            engines: vec![desc()],
        })?)
    }

    fn call(&self, header: &[u8], _body: &[u8]) -> anyhow::Result<Vec<u8>> {
        let payload = match decode_request(header)? {
            Request::Init { .. } => Payload::Init {
                protocol: PROTOCOL_VERSION,
                component_version: "1.0.0".into(),
            },
            Request::ProjectCheck { files, .. } => Payload::Project(vec![WireFileDiagnostics {
                path: files[0].path.clone(),
                diagnostics: vec![WireDiagnostic {
                    engine: "checker".into(),
                    code: Some("C001".into()),
                    severity: WireSeverity::Error,
                    title: "type error".into(),
                    description: None,
                    span: None,
                    url: None,
                    fix: Vec::new(),
                    metadata: Default::default(),
                }],
            }]),
            _ => Payload::Ok,
        };
        Ok(encode_response(&Response {
            result: Ok(payload),
            logs: Vec::new(),
        })?)
    }
}

struct Connector;

impl TransportConnector for Connector {
    fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        Ok(Arc::new(Checker))
    }
}

#[test]
fn a_component_served_project_check_reports_its_findings() {
    let cache = tempfile::tempdir().unwrap();
    let mut host_options = HostOptions::new(lock(), PathBuf::from(cache.path()));
    host_options.target = Some(TARGET.into());
    host_options.gate_loads = true;
    let host = ComponentHost::new(host_options)
        .unwrap()
        .with_connector(Arc::new(Connector));
    let options = RunOptions {
        component_injection: Some(ComponentInjection::new(host, index())),
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    };

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.py"), "x: int = 'a'\n").unwrap();
    let run = poly_core::lint_run(&[dir.path().to_path_buf()], &Config::default(), &options, false, false).unwrap();

    assert!(run.errors.is_empty(), "{:?}", run.errors);
    let findings: Vec<(&str, Option<&str>)> = run
        .results
        .iter()
        .flat_map(|result| result.diagnostics.iter())
        .filter(|d| d.engine == "checker")
        .map(|d| (d.engine.as_str(), d.code.as_deref()))
        .collect();
    assert_eq!(findings, [("checker", Some("C001"))], "results: {:?}", run.results);
}
