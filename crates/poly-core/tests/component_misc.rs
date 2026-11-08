//! The `misc` component driven through the full runner, as a core build runs it: the real producer
//! behind the transport and the committed index entry, with its families marked unlinked so the
//! planner takes them from the component. Each engine must lint what the in-process engine lints.
//! Runs in every build: the core (`--features core`) is the one that takes `misc` from a component.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alef_component_runtime::ComponentManager;
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
use poly_component_index::ComponentIndex;
use poly_core::components::ComponentInjection;
use poly_core::{Config, RunOptions};
use poly_engines::{MiscComponent, PolyComponent};

const TARGET: &str = "test-target";

struct Misc(MiscComponent);

impl ComponentTransport for Misc {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.describe()?)
    }

    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.call(header, body)?)
    }
}

struct Connector;

impl TransportConnector for Connector {
    fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        Ok(Arc::new(Misc(MiscComponent::default())))
    }
}

fn lock() -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"misc","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://example.invalid/misc.tar.gz",
            "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32),
        sha = "ab".repeat(32)
    )
}

/// The committed `misc` entry alone, under a feature name no build links.
fn index() -> ComponentIndex {
    let mut misc = poly_component_index::index()
        .component("misc")
        .expect("the index has misc")
        .clone();
    misc.features = vec!["engine-misc-under-test".into()];
    ComponentIndex {
        schema: 1,
        components: vec![misc],
        presets: Default::default(),
    }
}

fn options(cache: &Path) -> RunOptions {
    let mut host_options = HostOptions::new(lock(), PathBuf::from(cache));
    host_options.target = Some(TARGET.into());
    host_options.gate_loads = true;
    let host = ComponentHost::new(host_options)
        .unwrap()
        .with_connector(Arc::new(Connector));
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        component_injection: Some(ComponentInjection::new(host, index())),
        ..RunOptions::default()
    }
}

#[test]
fn ini_is_linted_through_the_misc_component() {
    let cache = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfg.ini"), "[a]\nk=v\n[a]\nk=again\n").unwrap();
    let run = poly_core::lint_run(
        &[dir.path().to_path_buf()],
        &Config::default(),
        &options(cache.path()),
        false,
        false,
    )
    .unwrap();
    assert_eq!(run.components.used, ["misc"]);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert!(run.skipped.is_empty(), "{:?}", run.skipped);
    assert_eq!(run.checked, 1);
    let codes: Vec<_> = run
        .results
        .iter()
        .flat_map(|r| &r.diagnostics)
        .filter(|d| d.engine == "ini")
        .filter_map(|d| d.code.as_deref())
        .collect();
    assert!(codes.contains(&"duplicate-section"), "{codes:?}");
}
