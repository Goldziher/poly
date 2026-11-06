//! A sidecar component (an executable on alef's framed stdio) behind the same `ComponentEngine`
//! proxy as a dylib.
//!
//! The failure modes run against a scripted stand-in (`tests/sidecar/fake.py`); the conformance
//! tests run against the real `sh` sidecar when `POLY_SH_SIDECAR` names its executable. CI builds
//! it and sets `POLY_REQUIRE_SIDECAR`, which turns a missing executable into a failure instead of a
//! skip.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use alef_component_runtime::ComponentManifest;
use common::{config, desc, describe_of, source};
use poly_component_host::{AlefConnector, ComponentEngine, ComponentHost, HostOptions, TransportConnector};
use poly_component_protocol::sidecar::METHOD_CALL;
use poly_component_protocol::{
    Describe, PROTOCOL_VERSION, Payload, Request, Response, WireErrorKind, WireFormatOutput, decode_describe,
    decode_response, encode_describe, encode_request, encode_response,
};
use poly_engine_api::engine::{Engine, FormatOutput, SourceFile};
use poly_engine_api::language::Language;
use serde_json::json;
use sha2::{Digest, Sha256};

const COMPONENT: &str = "sh";
const TARGET: &str = "test-target";
const FAKE: &str = include_str!("sidecar/fake.py");

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// An unsigned archive holding `executable`, and a lock that points at it.
fn package(executable: &[u8], dir: &Path) -> String {
    let provides = json!([{
        "contract": "poly", "interface_version": 1, "contract_hash": "11".repeat(32), "implementation": "NewShComponent"
    }]);
    let identity = json!({
        "crate_name": "poly", "component": COMPONENT, "version": "1.2.3", "target": TARGET,
        "feature_hash": "33".repeat(32), "contract_hash": "11".repeat(32)
    });
    let manifest: ComponentManifest = serde_json::from_value(json!({
        "schema_version": 2, "abi_version": 1, "identity": identity, "provides": provides,
        "features": [], "default_features": false,
        "library": {"file": "sidecar", "sha256": digest(executable), "size": executable.len()}
    }))
    .unwrap();
    let manifest_bytes = manifest.canonical_bytes().unwrap();
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in [("component.json", &manifest_bytes[..]), ("sidecar", executable)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o700);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes).unwrap();
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut gz, &builder.into_inner().unwrap()).unwrap();
    let archive = gz.finish().unwrap();
    let path = dir.join("component.tar.gz");
    std::fs::write(&path, &archive).unwrap();
    json!({
        "schema_version": 2, "public_keys": {},
        "artifacts": [{
            "identity": identity, "provides": provides, "mode": "download",
            "url": format!("file://{}", path.display()), "sha256": digest(&archive), "size": archive.len(),
            "manifest_sha256": digest(&manifest_bytes), "key_id": ""
        }]
    })
    .to_string()
}

fn host(lock: String, cache: &Path, timeout: Duration, offline: bool) -> ComponentHost {
    let mut options = HostOptions::new(lock, cache);
    options.target = Some(TARGET.to_owned());
    options.sidecars = vec![COMPONENT.to_owned()];
    options.sidecar_timeout = Some(timeout);
    options.offline = offline;
    ComponentHost::new(options).unwrap()
}

fn shell(text: &str) -> SourceFile {
    SourceFile {
        language: Language::Shell,
        path: PathBuf::from("run.sh"),
        ..source(text)
    }
}

fn shell_desc() -> poly_component_protocol::WireEngineDesc {
    let mut wire = desc(0, "none");
    wire.name = "shfmt".into();
    wire.languages = vec!["shell".into()];
    wire.capabilities.lint = false;
    wire
}

fn response(payload: Payload) -> Vec<u8> {
    encode_response(&Response {
        result: Ok(payload),
        logs: Vec::new(),
    })
    .unwrap()
}

/// The scripted stand-in, with `scenario` and its canned answers filled in.
fn fake(scenario: &str, init_protocol: u32, describe: &Describe) -> Vec<u8> {
    FAKE.replace("__SCENARIO__", scenario)
        .replace(
            "__INIT__",
            &hex::encode(response(Payload::Init {
                protocol: init_protocol,
                component_version: "1.2.3".into(),
            })),
        )
        .replace("__OK__", &hex::encode(response(Payload::Ok)))
        .replace("__DESCRIBE__", &hex::encode(encode_describe(describe).unwrap()))
        .replace(
            "__FORMATTED__",
            &hex::encode(response(Payload::Formatted(WireFormatOutput::Formatted(
                "FORMATTED".into(),
            )))),
        )
        .into_bytes()
}

struct Fixture {
    _dir: tempfile::TempDir,
    engine: ComponentEngine,
}

fn scripted(scenario: &str, init_protocol: u32, timeout: Duration, advertised: Option<Describe>) -> Fixture {
    let expected = describe_of(shell_desc());
    let served = advertised.unwrap_or_else(|| expected.clone());
    let dir = tempfile::tempdir().unwrap();
    let lock = package(&fake(scenario, init_protocol, &served), dir.path());
    let host = host(lock, &dir.path().join("cache"), timeout, false);
    host.ensure(COMPONENT, true);
    let engine = host.engine(COMPONENT, &shell_desc(), Some(expected)).unwrap();
    Fixture { _dir: dir, engine }
}

fn formatted(engine: &ComponentEngine) -> anyhow::Result<String> {
    match engine.format(&shell("echo  hi"), &config(2))? {
        FormatOutput::Formatted(text) => Ok(text),
        FormatOutput::Unchanged => Ok(String::new()),
    }
}

#[test]
fn a_sidecar_serves_a_format_call_through_the_proxy() {
    let fixture = scripted("ok", PROTOCOL_VERSION, Duration::from_secs(10), None);
    assert_eq!(formatted(&fixture.engine).unwrap(), "FORMATTED");
    assert_eq!(formatted(&fixture.engine).unwrap(), "FORMATTED");
}

#[test]
fn a_crash_mid_call_fails_that_call_and_every_later_one() {
    let fixture = scripted("crash_at_3", PROTOCOL_VERSION, Duration::from_secs(10), None);
    let first = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(first.contains("Format failed"), "{first}");
    let second = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(
        second.contains("stopped"),
        "a dead sidecar is never restarted: {second}"
    );
}

#[test]
fn a_hanging_call_times_out_and_stops_the_sidecar() {
    let fixture = scripted("hang_at_3", PROTOCOL_VERSION, Duration::from_millis(500), None);
    let start = Instant::now();
    let error = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "timed out late: {:?}",
        start.elapsed()
    );
    assert!(
        error.to_lowercase().contains("timed out") || error.contains("timeout"),
        "{error}"
    );
    assert!(formatted(&fixture.engine).is_err(), "the sidecar must stay stopped");
}

#[test]
fn a_status_error_is_reported_and_the_sidecar_stays_usable() {
    let fixture = scripted("status_error_at_3", PROTOCOL_VERSION, Duration::from_secs(10), None);
    let error = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(error.contains("producer failed"), "{error}");
    assert_eq!(formatted(&fixture.engine).unwrap(), "FORMATTED");
}

#[test]
fn an_undecodable_response_is_a_call_error_and_the_sidecar_stays_usable() {
    let fixture = scripted("garbage_at_3", PROTOCOL_VERSION, Duration::from_secs(10), None);
    let error = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(error.contains("malformed response"), "{error}");
    assert_eq!(formatted(&fixture.engine).unwrap(), "FORMATTED");
}

#[test]
fn a_protocol_version_mismatch_fails_the_load_without_a_fallback() {
    let fixture = scripted("ok", PROTOCOL_VERSION + 1, Duration::from_secs(10), None);
    let error = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(error.contains("speaks protocol"), "{error}");
    // The failure is remembered for the run: asking again gives the same answer.
    assert_eq!(format!("{:#}", formatted(&fixture.engine).unwrap_err()), error);
}

#[test]
fn a_describe_that_differs_from_the_index_fails_the_load() {
    let mut other = shell_desc();
    other.version = "9.9.9".into();
    let fixture = scripted(
        "ok",
        PROTOCOL_VERSION,
        Duration::from_secs(10),
        Some(describe_of(other)),
    );
    let error = format!("{:#}", formatted(&fixture.engine).unwrap_err());
    assert!(error.contains("describes itself differently"), "{error}");
}

#[test]
fn ensure_installs_a_sidecar_once_and_finds_it_cached_offline() {
    let dir = tempfile::tempdir().unwrap();
    let describe = describe_of(shell_desc());
    let lock = package(&fake("ok", PROTOCOL_VERSION, &describe), dir.path());
    let cache = dir.path().join("cache");
    let first = host(lock.clone(), &cache, Duration::from_secs(10), false).ensure(COMPONENT, true);
    assert_eq!(first, poly_component_host::Availability::InstalledNow, "{first:?}");
    let second = host(lock, &cache, Duration::from_secs(10), true).ensure(COMPONENT, true);
    assert_eq!(second, poly_component_host::Availability::Present, "{second:?}");
}

#[test]
fn a_sidecar_whose_handshake_disagrees_with_the_manifest_is_incompatible() {
    let dir = tempfile::tempdir().unwrap();
    // The stand-in copies its descriptor from component.json, so corrupt the script to lie.
    let lying = String::from_utf8(fake("ok", PROTOCOL_VERSION, &describe_of(shell_desc())))
        .unwrap()
        .replace(
            "\"component_id\": manifest",
            "\"component_id\": \"other\", \"x\": manifest",
        );
    let lock = package(lying.as_bytes(), dir.path());
    let answer = host(lock, &dir.path().join("cache"), Duration::from_secs(10), false).ensure(COMPONENT, true);
    assert!(
        matches!(&answer, poly_component_host::Availability::Unavailable(u)
            if u.kind == poly_component_host::UnavailableKind::Incompatible),
        "{answer:?}"
    );
}

mod real {
    //! The `sh` sidecar built from `components/sidecars/sh`.

    use super::*;

    fn executable() -> Option<Vec<u8>> {
        let Some(path) = std::env::var_os("POLY_SH_SIDECAR") else {
            assert!(
                std::env::var_os("POLY_REQUIRE_SIDECAR").is_none(),
                "POLY_REQUIRE_SIDECAR is set but POLY_SH_SIDECAR names no executable: the sidecar tests were skipped"
            );
            eprintln!("skipped: POLY_SH_SIDECAR is not set (build components/sidecars/sh and point it at the binary)");
            return None;
        };
        Some(std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}")))
    }

    struct Real {
        _dir: tempfile::TempDir,
        host: ComponentHost,
        describe: Describe,
    }

    fn start() -> Option<Real> {
        let exe = executable()?;
        let dir = tempfile::tempdir().unwrap();
        let lock = package(&exe, dir.path());
        let host = host(lock, &dir.path().join("cache"), Duration::from_secs(20), false);
        let available = host.ensure(COMPONENT, true);
        assert!(available.is_usable(), "{available:?}");
        let transport = AlefConnector::with_sidecars([COMPONENT.to_owned()])
            .connect(host.manager(), COMPONENT)
            .unwrap();
        let describe = decode_describe(&transport.describe().unwrap()).unwrap();
        Some(Real {
            _dir: dir,
            host,
            describe,
        })
    }

    impl Real {
        fn engine(&self) -> ComponentEngine {
            let desc = &self.describe.engines[0];
            self.host.engine(COMPONENT, desc, Some(self.describe.clone())).unwrap()
        }
    }

    fn format(engine: &ComponentEngine, text: &str, indent: usize, tabs: bool) -> String {
        let mut cfg = config(indent);
        if tabs {
            cfg.options.insert("use_tabs".into(), toml::Value::Boolean(true));
        }
        match engine.format(&shell(text), &cfg).unwrap() {
            FormatOutput::Formatted(out) => out,
            FormatOutput::Unchanged => text.to_owned(),
        }
    }

    /// The archive `alef component package --library` makes, installed and spawned through the real
    /// manager: the sidecar reads the identity it answers the handshake with from the `component.json`
    /// alef installed next to it. `POLY_SH_SIDECAR_LOCK` names the lock `alef component lock` wrote.
    #[test]
    fn an_alef_packaged_archive_passes_the_manifest_handshake() {
        let Some(path) = std::env::var_os("POLY_SH_SIDECAR_LOCK") else {
            assert!(
                std::env::var_os("POLY_REQUIRE_SIDECAR").is_none(),
                "POLY_REQUIRE_SIDECAR is set but POLY_SH_SIDECAR_LOCK is not: the packaged archive was not tested"
            );
            eprintln!("skipped: POLY_SH_SIDECAR_LOCK is not set");
            return;
        };
        let lock = std::fs::read_to_string(&path).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let mut options = HostOptions::new(lock, cache.path());
        options.sidecars = vec![COMPONENT.to_owned()];
        options.sidecar_timeout = Some(Duration::from_secs(20));
        let host = ComponentHost::new(options).unwrap();
        let available = host.ensure(COMPONENT, true);
        assert!(available.is_usable(), "{available:?}");
        let transport = AlefConnector::with_sidecars([COMPONENT.to_owned()])
            .connect(host.manager(), COMPONENT)
            .unwrap();
        let describe = decode_describe(&transport.describe().unwrap()).unwrap();
        assert_eq!(describe.engines[0].name, "shfmt");
        let committed = poly_component_index::index()
            .component("sh")
            .expect("the index lists sh");
        assert_eq!(
            describe.engines, committed.engines,
            "the sidecar's describe() must equal its entry in the component index"
        );
    }

    #[test]
    fn describes_one_format_only_shfmt_engine() {
        let Some(real) = start() else { return };
        assert_eq!(real.describe.protocol, PROTOCOL_VERSION);
        let [engine] = real.describe.engines.as_slice() else {
            panic!("{:?}", real.describe.engines)
        };
        assert_eq!(engine.name, "shfmt");
        assert_eq!(engine.languages, ["shell"]);
        assert!(engine.capabilities.format && !engine.capabilities.lint);
        assert_eq!(engine.host_hook, "editorconfig");
    }

    #[test]
    fn formats_with_the_configured_indent() {
        let Some(real) = start() else { return };
        let engine = real.engine();
        let src = "greet() {\n        echo hi\n}\n";
        assert_eq!(format(&engine, src, 2, false), "greet() {\n  echo hi\n}\n");
        assert_eq!(format(&engine, src, 4, false), "greet() {\n    echo hi\n}\n");
        assert_eq!(format(&engine, src, 2, true), "greet() {\n\techo hi\n}\n");
        assert!(matches!(
            engine.format(&shell("echo hi\n"), &config(2)).unwrap(),
            FormatOutput::Unchanged
        ));
    }

    #[test]
    fn a_syntax_error_leaves_the_file_alone() {
        let Some(real) = start() else { return };
        let out = real
            .engine()
            .format(&shell("if [ 1 ]; then\n  echo \"open\n"), &config(2))
            .unwrap();
        assert!(matches!(out, FormatOutput::Unchanged));
    }

    #[test]
    fn an_error_response_and_a_status_error_leave_the_process_usable() {
        let Some(real) = start() else { return };
        let engine = real.engine();
        let slot = real.host.slot(COMPONENT, None).unwrap();
        let connection = slot.connection().unwrap();
        // Lint is not served: a Response carrying `Unsupported`, status 0.
        let error = connection
            .call_plain(
                "shfmt",
                &Request::BatchSupport {
                    engine: "shfmt".into(),
                    config: 1,
                },
                b"",
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("does not support"), "{error:#}");
        // A payload the sidecar cannot even split is a non-zero stdio status.
        let transport = AlefConnector::with_sidecars([COMPONENT.to_owned()])
            .connect(real.host.manager(), COMPONENT)
            .unwrap();
        let bad = transport.call(b"\xc1", b"").unwrap_err();
        assert!(format!("{bad:#}").contains("decode request"), "{bad:#}");
        let short = real.host.manager().ensure_contract(COMPONENT, "poly", b"").unwrap();
        let truncated = short.invoke_bytes(METHOD_CALL, &[1, 0]).unwrap_err();
        assert!(truncated.to_string().contains("4-byte"), "{truncated}");
        assert_eq!(
            format(&engine, "if true; then\necho  hi\nfi\n", 2, false),
            "if true; then\n  echo hi\nfi\n"
        );
    }

    #[test]
    fn an_unknown_engine_is_a_wire_error_not_a_crash() {
        let Some(real) = start() else { return };
        let transport = AlefConnector::with_sidecars([COMPONENT.to_owned()])
            .connect(real.host.manager(), COMPONENT)
            .unwrap();
        let header = encode_request(&Request::RegisterConfig {
            id: 1,
            config: poly_engine_api::component_wire::engine_config_to_wire(&config(2)),
        })
        .unwrap();
        let ok = decode_response(&transport.call(&header, b"").unwrap()).unwrap();
        assert!(ok.result.is_ok());
        let file = poly_component_protocol::WireFile {
            path: "a.sh".into(),
            language: "shell".into(),
            len: 1,
        };
        let header = encode_request(&Request::Format {
            engine: "nope".into(),
            config: 1,
            file,
        })
        .unwrap();
        let answer = decode_response(&transport.call(&header, b"x").unwrap()).unwrap();
        assert_eq!(answer.result.unwrap_err().kind, WireErrorKind::UnknownEngine);
    }

    fn testdata() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../components/sidecars/sh/testdata")
    }

    /// The sidecar's output is the host `shfmt -i <width>` output, byte for byte, over the fixtures.
    #[test]
    fn matches_the_host_shfmt_on_every_fixture() {
        let Some(real) = start() else { return };
        let Ok(version) = std::process::Command::new("shfmt").arg("--version").output() else {
            assert!(
                std::env::var_os("POLY_REQUIRE_SIDECAR").is_none(),
                "POLY_REQUIRE_SIDECAR is set but shfmt is not on PATH, so parity was not checked"
            );
            eprintln!("skipped: shfmt is not on PATH");
            return;
        };
        eprintln!("host shfmt {}", String::from_utf8_lossy(&version.stdout).trim());
        let engine = real.engine();
        let mut compared = 0;
        for entry in std::fs::read_dir(testdata()).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() || path.file_name().is_some_and(|n| n == "broken.sh") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (indent, tabs) in [(2, false), (4, false), (2, true)] {
                let width = if tabs { 0 } else { indent };
                let mut child = std::process::Command::new("shfmt")
                    .args(["-i", &width.to_string()])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                std::io::Write::write_all(child.stdin.as_mut().unwrap(), text.as_bytes()).unwrap();
                let expected = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
                let src = SourceFile {
                    path: path.clone(),
                    ..shell(&text)
                };
                let mut cfg = config(indent);
                if tabs {
                    cfg.options.insert("use_tabs".into(), toml::Value::Boolean(true));
                }
                let got = match engine.format(&src, &cfg).unwrap() {
                    FormatOutput::Formatted(out) => out,
                    FormatOutput::Unchanged => text.clone(),
                };
                assert_eq!(got, expected, "{} at indent {width}", path.display());
                compared += 1;
            }
        }
        assert!(compared >= 12, "compared only {compared} fixture/option pairs");
    }
}
