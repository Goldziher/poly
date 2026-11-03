//! `ComponentHost` over a fake connector and a lock with one component.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alef_component_runtime::ComponentManager;
use common::{FakeComponent, desc, describe_of};
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, SlotStatus, TransportConnector};
use poly_engine_api::engine::Engine;

const TARGET: &str = "test-target";

fn lock(sha: &str) -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"demo-component","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://example.invalid/demo.tar.gz",
            "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32)
    )
}

struct Connector {
    connects: AtomicUsize,
    fail: bool,
    fake: Arc<FakeComponent>,
}

impl TransportConnector for Connector {
    fn connect(&self, _: &ComponentManager, id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        assert_eq!(id, "demo-component");
        if self.fail {
            anyhow::bail!("download failed");
        }
        Ok(self.fake.clone())
    }
}

fn host(sha: &str, fail: bool) -> (ComponentHost, Arc<Connector>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut options = HostOptions::new(lock(sha), PathBuf::from(dir.path()));
    options.target = Some(TARGET.into());
    options.offline = true;
    let connector = Arc::new(Connector {
        connects: AtomicUsize::new(0),
        fail,
        fake: FakeComponent::new(describe_of(desc(0, "none"))),
    });
    let host = ComponentHost::new(options).unwrap().with_connector(connector.clone());
    (host, connector, dir)
}

#[test]
fn engines_load_lazily_and_share_one_slot() {
    let (host, connector, _dir) = host(&"ab".repeat(32), false);
    let wire = desc(0, "none");
    let first = host.engine("demo-component", &wire, None).unwrap();
    let second = host.engine("demo-component", &wire, None).unwrap();
    assert_eq!(connector.connects.load(Ordering::SeqCst), 0);
    assert_eq!(first.version(), format!("1.2.3+c{}", "ab".repeat(8)));
    let src = common::source("x");
    first.lint(&src, &common::config(4)).unwrap();
    second.lint(&src, &common::config(4)).unwrap();
    assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
    assert_eq!(first.slot().status(), SlotStatus::Ready);
}

#[test]
fn a_failed_load_is_remembered() {
    let (host, connector, _dir) = host(&"ab".repeat(32), true);
    let engine = host.engine("demo-component", &desc(0, "none"), None).unwrap();
    let src = common::source("x");
    let first = engine.lint(&src, &common::config(4)).unwrap_err().to_string();
    let second = engine.lint(&src, &common::config(4)).unwrap_err().to_string();
    assert!(first.contains("download failed"), "{first}");
    assert_eq!(first, second);
    assert_eq!(connector.connects.load(Ordering::SeqCst), 1);
}

#[test]
fn the_lock_digest_changes_the_engine_version() {
    let (a, _, _d1) = host(&"aa".repeat(32), false);
    let (b, _, _d2) = host(&"bb".repeat(32), false);
    let wire = desc(0, "none");
    let va = a.engine("demo-component", &wire, None).unwrap().version().to_owned();
    let vb = b.engine("demo-component", &wire, None).unwrap().version().to_owned();
    assert_ne!(va, vb);
}

#[test]
fn unknown_component_is_an_error_and_unavailable_engines_still_build() {
    let (host, _, _dir) = host(&"ab".repeat(32), false);
    let wire = desc(0, "none");
    assert!(host.engine("missing", &wire, None).is_err());
    let engine = host
        .unavailable_engine("missing", &wire, "no artifact for this target")
        .unwrap();
    assert_eq!(engine.version(), "1.2.3+c0000000000000000");
    assert_eq!(engine.unavailable_reason(), Some("no artifact for this target"));
}
