//! A project check served by a downloadable component runs in the project phase.
//!
//! The component is a fake `checker` engine for Python with the `project` capability, behind an injected
//! host and index, so the run is a core build serving the engine from a component rather than linking it.
#![cfg(feature = "all-engines")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use alef_component_runtime::ComponentManager;
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_component_protocol::{
    Describe, PROTOCOL_VERSION, Payload, Request, Response, WireCapabilities, WireDiagnostic, WireEngineDesc,
    WireFileDiagnostics, WireProjectDirs, WireProjectInputs, WireSeverity, decode_request, encode_describe,
    encode_response,
};
use poly_core::components::ComponentInjection;
use poly_core::{Config, RunOptions};

const TARGET: &str = "test-target";

/// The `threads` of the last `ProjectCheck` the component saw; 0 when it carried none.
static SEEN_THREADS: AtomicU32 = AtomicU32::new(u32::MAX);

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
        project_inputs: Default::default(),
        selection: None,
    }
}

fn index() -> ComponentIndex {
    ComponentIndex {
        schema: 1,
        components: vec![Component {
            name: "checker".into(),
            kind: ComponentKind::Engines,
            opt_in: false,
            download_only: false,
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
            Request::ProjectCheck { files, threads, .. } => {
                SEEN_THREADS.store(threads.unwrap_or(0), Ordering::SeqCst);
                Payload::Project(vec![WireFileDiagnostics {
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
                }])
            }
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
    assert_eq!(
        SEEN_THREADS.load(Ordering::SeqCst),
        1,
        "the run's `jobs = 1` must reach the component's project check"
    );
}

/// The file names of every `ProjectCheck` the recording component received.
static SEEN_CALLS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

fn whole_tree_desc() -> WireEngineDesc {
    WireEngineDesc {
        project_inputs: WireProjectInputs {
            gate_key: None,
            files: Vec::new(),
            dirs: Some(WireProjectDirs {
                dir_key: "dir".into(),
                dir_default: ".".into(),
                paths_key: None,
                recursive_key: None,
                recursive_default: true,
                whole_tree: true,
                file_names: Vec::new(),
                list_unserved: false,
            }),
        },
        ..desc()
    }
}

/// Records the files of each project check and reports nothing.
struct Recorder;

impl ComponentTransport for Recorder {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(encode_describe(&Describe {
            protocol: PROTOCOL_VERSION,
            component_version: "1.0.0".into(),
            engines: vec![whole_tree_desc()],
        })?)
    }

    fn call(&self, header: &[u8], _body: &[u8]) -> anyhow::Result<Vec<u8>> {
        let payload = match decode_request(header)? {
            Request::Init { .. } => Payload::Init {
                protocol: PROTOCOL_VERSION,
                component_version: "1.0.0".into(),
            },
            Request::ProjectCheck { files, .. } => {
                SEEN_CALLS
                    .lock()
                    .unwrap()
                    .push(files.iter().map(|f| f.path.clone()).collect());
                Payload::Project(Vec::new())
            }
            _ => Payload::Ok,
        };
        Ok(encode_response(&Response {
            result: Ok(payload),
            logs: Vec::new(),
        })?)
    }
}

struct RecorderConnector;

impl TransportConnector for RecorderConnector {
    fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        Ok(Arc::new(Recorder))
    }
}

#[test]
fn a_run_on_a_nested_directory_gives_a_component_the_whole_tree_of_the_ancestor_config() {
    let mut component_index = index();
    component_index.components[0].engines = vec![whole_tree_desc()];
    let cache = tempfile::tempdir().unwrap();
    let mut host_options = HostOptions::new(lock(), PathBuf::from(cache.path()));
    host_options.target = Some(TARGET.into());
    host_options.gate_loads = true;
    let host = ComponentHost::new(host_options)
        .unwrap()
        .with_connector(Arc::new(RecorderConnector));
    let options = RunOptions {
        component_injection: Some(ComponentInjection::new(host, component_index)),
        no_cache: true,
        jobs: Some(1),
        ..RunOptions::default()
    };

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, contents: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write("poly.toml", "[workspace]\nroot = true\n");
    write("packages/a/poly.toml", "[format]\n");
    write("packages/a/a.py", "a = 1\n");
    write("packages/b/b.py", "b = 1\n");
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();

    SEEN_CALLS.lock().unwrap().clear();
    let run = poly_core::lint_run(
        &[dir.path().join("packages/a")],
        &Config::default(),
        &options,
        false,
        false,
    )
    .unwrap();
    assert!(run.errors.is_empty(), "{:?}", run.errors);

    let calls = SEEN_CALLS.lock().unwrap().clone();
    let mut seen: Vec<String> = calls.concat();
    seen.sort();
    let names: Vec<&str> = seen.iter().map(|p| p.rsplit('/').next().unwrap()).collect();
    assert_eq!(
        names,
        ["a.py", "b.py"],
        "the root config judges the whole tree: {calls:?}"
    );
}
