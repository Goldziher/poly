//! `ComponentHost::ensure`: install policy, remembered answers and the load gate.

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alef_component_runtime::{ComponentError, ComponentManager};
use common::{FakeComponent, desc, describe_of};
use poly_component_host::{
    Availability, ComponentHost, ComponentTransport, HostOptions, TransportConnector, UnavailableKind,
};
use poly_engine_api::engine::Engine;

const TARGET: &str = "test-target";

fn lock() -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"demo-component","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://example.invalid/demo.tar.gz",
            "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32),
        sha = "ab".repeat(32)
    )
}

#[derive(Clone, Copy)]
enum Mode {
    Works,
    DownloadError,
    DigestMismatch,
    AbiMismatch,
    /// The archive lands in the cache, then the connector's own checks reject the component.
    RejectedAfterDownload,
}

struct Connector {
    connects: AtomicUsize,
    mode: Mode,
    fake: Arc<FakeComponent>,
}

impl TransportConnector for Connector {
    fn connect(&self, manager: &ComponentManager, id: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Works => Ok(self.fake.clone()),
            Mode::DownloadError => anyhow::bail!("connection reset"),
            Mode::DigestMismatch => Err(ComponentError::DigestMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }
            .into()),
            Mode::RejectedAfterDownload => {
                let root = manager.cache_path(id)?;
                std::fs::create_dir_all(&root)?;
                std::fs::write(root.join("component.json"), "{}")?;
                anyhow::bail!("component describes itself differently from the index")
            }
            Mode::AbiMismatch => Err(ComponentError::IncompatibleAbi {
                expected_major: 1,
                expected_minor: 0,
                actual_major: 2,
                actual_minor: 0,
            }
            .into()),
        }
    }
}

struct Fixture {
    host: ComponentHost,
    connector: Arc<Connector>,
    _dir: tempfile::TempDir,
}

fn fixture(mode: Mode, offline: bool, gate: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut options = HostOptions::new(lock(), PathBuf::from(dir.path()));
    options.target = Some(TARGET.into());
    options.offline = offline;
    options.gate_loads = gate;
    let connector = Arc::new(Connector {
        connects: AtomicUsize::new(0),
        mode,
        fake: FakeComponent::new(describe_of(desc(0, "none"))),
    });
    let host = ComponentHost::new(options).unwrap().with_connector(connector.clone());
    Fixture {
        host,
        connector,
        _dir: dir,
    }
}

fn unavailable_kind(availability: Availability) -> UnavailableKind {
    match availability {
        Availability::Unavailable(unavailable) => unavailable.kind,
        other => panic!("expected unavailable, got {other:?}"),
    }
}

#[test]
fn first_use_installs_through_the_connector_and_the_answer_is_remembered() {
    let f = fixture(Mode::Works, false, true);
    assert_eq!(f.host.ensure("demo-component", true), Availability::InstalledNow);
    assert_eq!(f.host.ensure("demo-component", true), Availability::InstalledNow);
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 1);
    let engine = f.host.engine("demo-component", &desc(0, "none"), None).unwrap();
    engine
        .lint(&common::source("x"), &common::config(4))
        .expect("the ensured transport is reused");
    assert_eq!(
        f.connector.connects.load(Ordering::SeqCst),
        1,
        "the load did not reconnect"
    );
}

#[test]
fn offline_never_connects() {
    let f = fixture(Mode::Works, true, true);
    let availability = f.host.ensure("demo-component", true);
    assert_eq!(unavailable_kind(availability.clone()), UnavailableKind::Offline);
    let Availability::Unavailable(unavailable) = availability else {
        unreachable!()
    };
    assert_eq!(
        unavailable.reason("demo-component"),
        "component not installed: demo-component (offline)"
    );
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn install_never_never_connects() {
    let f = fixture(Mode::Works, false, true);
    assert_eq!(
        unavailable_kind(f.host.ensure("demo-component", false)),
        UnavailableKind::InstallNever
    );
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn failures_are_classified() {
    let cases = [
        (Mode::DownloadError, UnavailableKind::DownloadFailed),
        (Mode::DigestMismatch, UnavailableKind::VerifyFailed),
        (Mode::AbiMismatch, UnavailableKind::Incompatible),
        (Mode::RejectedAfterDownload, UnavailableKind::Incompatible),
    ];
    for (mode, expected) in cases {
        let f = fixture(mode, false, true);
        assert_eq!(unavailable_kind(f.host.ensure("demo-component", true)), expected);
    }
    let f = fixture(Mode::Works, false, true);
    assert_eq!(
        unavailable_kind(f.host.ensure("not-in-the-lock", true)),
        UnavailableKind::Incompatible
    );
}

#[test]
fn a_gated_host_never_loads_an_unensured_component() {
    let f = fixture(Mode::Works, false, true);
    let engine = f.host.engine("demo-component", &desc(0, "none"), None).unwrap();
    let error = engine
        .lint(&common::source("x"), &common::config(4))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not made available"), "{error}");
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn a_gated_host_does_not_load_a_component_that_failed_to_install() {
    let f = fixture(Mode::DownloadError, false, true);
    assert!(!f.host.ensure("demo-component", true).is_usable());
    let engine = f.host.engine("demo-component", &desc(0, "none"), None).unwrap();
    assert!(engine.lint(&common::source("x"), &common::config(4)).is_err());
    assert_eq!(
        f.connector.connects.load(Ordering::SeqCst),
        1,
        "only the ensure connected"
    );
}

#[test]
fn an_ungated_host_still_loads_lazily() {
    let f = fixture(Mode::Works, false, false);
    let engine = f.host.engine("demo-component", &desc(0, "none"), None).unwrap();
    engine.lint(&common::source("x"), &common::config(4)).unwrap();
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 1);
}

#[test]
fn a_sealed_host_refuses_new_installs_but_keeps_its_answers() {
    let f = fixture(Mode::Works, false, true);
    f.host.seal();
    assert_eq!(
        unavailable_kind(f.host.ensure("demo-component", true)),
        UnavailableKind::DownloadFailed
    );
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
    assert_eq!(f.host.availability("demo-component"), None);
}

#[test]
fn inspecting_a_host_never_installs() {
    let f = fixture(Mode::Works, false, true);
    let _ = f.host.status("demo-component");
    let _ = f.host.lock_sha256("demo-component");
    let _ = f.host.availability("demo-component");
    let _ = f.host.slot("demo-component", None).unwrap();
    let _ = f.host.engine("demo-component", &desc(0, "none"), None).unwrap();
    let _ = f
        .host
        .unavailable_engine("demo-component", &desc(0, "none"), "why")
        .unwrap();
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
}

#[test]
fn a_frontend_is_never_fetched_offline_or_under_install_never() {
    let offline = fixture(Mode::Works, true, true);
    assert_eq!(
        unavailable_kind(offline.host.ensure_frontend("demo-component", true)),
        UnavailableKind::Offline
    );
    let never = fixture(Mode::Works, false, true);
    assert_eq!(
        unavailable_kind(never.host.ensure_frontend("demo-component", false)),
        UnavailableKind::InstallNever
    );
    assert_eq!(
        never.connector.connects.load(Ordering::SeqCst),
        0,
        "a frontend is never connected as an engine component"
    );
}

#[test]
fn a_frontend_missing_from_the_lock_is_incompatible() {
    let f = fixture(Mode::Works, false, true);
    assert_eq!(
        unavailable_kind(f.host.ensure_frontend("not-in-the-lock", true)),
        UnavailableKind::Incompatible
    );
}

#[test]
fn a_frontend_that_was_not_ensured_is_not_run() {
    let f = fixture(Mode::Works, false, true);
    let error = f.host.run_frontend("demo-component", b"").unwrap_err();
    assert!(
        format!("{error:#}").contains("was not made available"),
        "an unensured frontend must fail before any load: {error:#}"
    );
}
