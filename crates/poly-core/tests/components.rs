//! The pre-plan pass end to end: a fake component serving `.lua`, an injected host and index.
//!
//! The compiled-in index names only components the default build links, so nothing in it is
//! downloadable; each test injects a one-component index (`demo`, engine `demo`, language
//! `lua`) and a host whose connector is a counting fake.
#![cfg(feature = "all-engines")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alef_component_runtime::{ComponentError, ComponentManager};
use poly_component_host::{ComponentHost, ComponentTransport, HostOptions, TransportConnector};
use poly_component_index::{Component, ComponentIndex, ComponentKind};
use poly_component_protocol::{
    Describe, PROTOCOL_VERSION, Payload, Request, Response, WireCapabilities, WireDiagnostic, WireEngineDesc,
    WireFormatOutput, WireSeverity, decode_request, encode_describe, encode_response,
};
use poly_config::v2::{ComponentInstallPolicy, ComponentsConfig, OnMissing};
use poly_core::components::ComponentInjection;
use poly_core::report::{FormatDocument, LintDocument};
use poly_core::{Config, RunOptions, is_withdrawal_reason};

const TARGET: &str = "test-target";

fn desc() -> WireEngineDesc {
    desc_for("lua")
}

fn desc_for(language: &str) -> WireEngineDesc {
    WireEngineDesc {
        name: "demo".into(),
        languages: vec![language.into()],
        capabilities: WireCapabilities {
            lint: true,
            format: true,
            fix: false,
            project: false,
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
    }
}

fn index() -> ComponentIndex {
    index_of(desc())
}

fn index_of(desc: WireEngineDesc) -> ComponentIndex {
    ComponentIndex {
        schema: 1,
        components: vec![Component {
            name: "demo".into(),
            kind: ComponentKind::Engines,
            opt_in: false,
            features: vec!["engine-demo".into()],
            engines: vec![desc],
            lock_sha: None,
        }],
        presets: Default::default(),
    }
}

fn lock() -> String {
    format!(
        r#"{{"schema_version":2,"public_keys":{{}},"artifacts":[{{
            "identity":{{"crate_name":"c","component":"demo","version":"1.0.0","target":"{TARGET}",
                         "feature_hash":"{h}","contract_hash":"{h}"}},
            "provides":[],"mode":"download","url":"https://example.invalid/demo.tar.gz",
            "sha256":"{sha}","size":1,"manifest_sha256":"{h}","key_id":"k"}}]}}"#,
        h = "11".repeat(32),
        sha = "ab".repeat(32)
    )
}

/// Speaks the protocol for `demo`: every lint finds one thing, every format is a no-op.
struct Demo;

impl ComponentTransport for Demo {
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
            Request::Lint { .. } => Payload::Diagnostics(vec![WireDiagnostic {
                engine: "demo".into(),
                code: Some("D001".into()),
                severity: WireSeverity::Warning,
                title: "demo finding".into(),
                description: None,
                span: None,
                url: None,
                fix: Vec::new(),
                metadata: Default::default(),
            }]),
            Request::Format { .. } => Payload::Formatted(WireFormatOutput::Unchanged),
            _ => Payload::Ok,
        };
        Ok(encode_response(&Response {
            result: Ok(payload),
            logs: Vec::new(),
        })?)
    }
}

/// Describes itself with another engine version than the index records.
struct Drifted;

impl ComponentTransport for Drifted {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        let mut engine = desc();
        engine.version = "9.9.9".into();
        Ok(encode_describe(&Describe {
            protocol: PROTOCOL_VERSION,
            component_version: "1.0.0".into(),
            engines: vec![engine],
        })?)
    }

    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        Demo.call(header, body)
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Works,
    Drifts,
    DownloadFails,
    Tampered,
}

struct Connector {
    connects: AtomicUsize,
    mode: Mode,
}

impl TransportConnector for Connector {
    fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Works => Ok(Arc::new(Demo)),
            Mode::Drifts => Ok(Arc::new(Drifted)),
            Mode::DownloadFails => anyhow::bail!("connection reset"),
            Mode::Tampered => Err(ComponentError::DigestMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }
            .into()),
        }
    }
}

struct Fixture {
    host: ComponentHost,
    connector: Arc<Connector>,
    _cache: tempfile::TempDir,
}

fn fixture(mode: Mode, offline: bool) -> Fixture {
    let cache = tempfile::tempdir().unwrap();
    let mut options = HostOptions::new(lock(), PathBuf::from(cache.path()));
    options.target = Some(TARGET.into());
    options.offline = offline;
    options.gate_loads = true;
    let connector = Arc::new(Connector {
        connects: AtomicUsize::new(0),
        mode,
    });
    let host = ComponentHost::new(options).unwrap().with_connector(connector.clone());
    Fixture {
        host,
        connector,
        _cache: cache,
    }
}

fn options() -> RunOptions {
    RunOptions {
        no_cache: true,
        jobs: Some(1),
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn config(install: ComponentInstallPolicy, on_missing: OnMissing) -> Config {
    Config {
        components: ComponentsConfig {
            install,
            on_missing,
            ..ComponentsConfig::default()
        },
        ..Config::default()
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.lua"), "print('x')\n").unwrap();
    dir
}

/// Run options that serve `host` and `index` to this run only.
fn inject(host: ComponentHost, index: ComponentIndex) -> RunOptions {
    RunOptions {
        component_injection: Some(ComponentInjection::new(host, index)),
        ..options()
    }
}

fn lint(dir: &Path, config: &Config, options: &RunOptions) -> poly_core::LintRun {
    poly_core::lint_run(&[dir.to_path_buf()], config, options, false, false).unwrap()
}

fn format(dir: &Path, config: &Config, options: &RunOptions) -> poly_core::FormatRun {
    poly_core::format_run(&[dir.to_path_buf()], config, options, false, false).unwrap()
}

fn default_config() -> Config {
    config(ComponentInstallPolicy::Auto, OnMissing::Fail)
}

#[test]
fn first_use_installs_the_component_and_lints_through_it() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let run = lint(dir.path(), &default_config(), &inject(f.host, index()));
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 1);
    assert_eq!(run.components.used, ["demo"]);
    assert_eq!(run.components.installed_now, ["demo"]);
    assert!(run.components.missing.is_empty());
    assert_eq!(run.checked, 1);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert_eq!(run.results.len(), 1);
    assert_eq!(run.results[0].diagnostics[0].engine, "demo");
    let json = serde_json::to_value(LintDocument::from_run(&run)).unwrap();
    assert_eq!(json["components"]["used"][0], "demo");
    assert_eq!(json["components"]["installed_now"][0], "demo");
    assert!(json["summary"].get("missing_components").is_none());
}

#[test]
fn a_second_run_makes_no_fetch() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let options = inject(f.host, index());
    lint(dir.path(), &default_config(), &options);
    let first = f.connector.connects.load(Ordering::SeqCst);
    let second = lint(dir.path(), &default_config(), &options);
    assert_eq!(second.components.used, ["demo"]);
    assert_eq!((first, f.connector.connects.load(Ordering::SeqCst)), (1, 1));
}

#[test]
fn offline_with_nothing_installed_fails_the_run_and_charges_the_skip() {
    let f = fixture(Mode::Works, true);
    let dir = project();
    let run = lint(dir.path(), &default_config(), &inject(f.host, index()));
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
    assert!(run.components.blocking(), "the CLI turns this into exit 2");
    let missing = &run.components.missing[0];
    assert_eq!((missing.name.as_str(), missing.kind.as_str()), ("demo", "offline"));
    assert_eq!(run.checked, 0);
    let reason = "component not installed: demo (offline)";
    assert_eq!(run.skipped.len(), 1);
    assert_eq!(run.skipped[0].reason, reason);
    assert!(
        !is_withdrawal_reason(reason),
        "a missing component is charged to --deny-skips"
    );
    let json = serde_json::to_value(LintDocument::from_run(&run)).unwrap();
    assert_eq!(json["summary"]["missing_components"], 1);
    assert_eq!(json["components"]["missing"][0]["blocking"], true);
}

#[test]
fn on_missing_skip_keeps_the_skip_and_stops_blocking() {
    let f = fixture(Mode::Works, true);
    let dir = project();
    let config = config(ComponentInstallPolicy::Auto, OnMissing::Skip);
    let run = lint(dir.path(), &config, &inject(f.host, index()));
    assert!(!run.components.blocking());
    assert_eq!(run.components.missing.len(), 1);
    assert_eq!(run.skipped.len(), 1, "still counted, so --deny-skips exits 2");
    assert_eq!(run.skipped[0].reason, "component not installed: demo (offline)");
}

#[test]
fn install_never_does_not_connect_and_says_why() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let config = config(ComponentInstallPolicy::Never, OnMissing::Skip);
    let run = lint(dir.path(), &config, &inject(f.host, index()));
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
    assert_eq!(run.skipped[0].reason, "component not installed: demo (install_never)");
    assert_eq!(run.components.missing[0].kind, "install_never");
}

#[test]
fn the_run_override_beats_the_config_install_policy() {
    let dir = project();

    let f = fixture(Mode::Works, false);
    let options = RunOptions {
        install: Some(ComponentInstallPolicy::Never),
        on_missing: Some(OnMissing::Skip),
        ..inject(f.host, index())
    };
    let run = lint(dir.path(), &default_config(), &options);
    assert_eq!(
        f.connector.connects.load(Ordering::SeqCst),
        0,
        "install = never downloads nothing"
    );
    assert_eq!(run.components.missing[0].kind, "install_never");
    assert!(!run.components.blocking());

    let f = fixture(Mode::Works, false);
    let options = RunOptions {
        install: Some(ComponentInstallPolicy::Auto),
        ..inject(f.host, index())
    };
    let config = config(ComponentInstallPolicy::Never, OnMissing::Fail);
    let run = lint(dir.path(), &config, &options);
    assert_eq!(
        f.connector.connects.load(Ordering::SeqCst),
        1,
        "install = auto downloads"
    );
    assert_eq!(run.components.installed_now, ["demo"]);
}

#[test]
fn a_failed_download_errors_each_file_and_blocks_even_under_skip() {
    let f = fixture(Mode::DownloadFails, false);
    let dir = project();
    let config = config(ComponentInstallPolicy::Auto, OnMissing::Skip);
    let run = lint(dir.path(), &config, &inject(f.host, index()));
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 1);
    assert_eq!(run.components.missing[0].kind, "download_failed");
    assert!(run.components.blocking());
    assert_eq!(run.errors.len(), 1);
    assert!(
        run.errors[0].message.contains("download_failed: demo"),
        "{}",
        run.errors[0].message
    );
}

#[test]
fn a_rejected_archive_is_a_verify_failure() {
    let f = fixture(Mode::Tampered, false);
    let dir = project();
    let run = lint(dir.path(), &default_config(), &inject(f.host, index()));
    assert_eq!(run.components.missing[0].kind, "verify_failed");
    assert_eq!(run.errors.len(), 1);
    assert!(
        run.errors[0].message.contains("verify_failed: demo"),
        "{}",
        run.errors[0].message
    );
}

#[test]
fn formatting_reports_the_same_account() {
    let installed = fixture(Mode::Works, false);
    let dir = project();
    let run = format(dir.path(), &default_config(), &inject(installed.host, index()));
    assert_eq!(run.components.used, ["demo"]);
    assert_eq!(run.checked, 1, "the component formatted it, not the tier-2 reindenter");
    let json = serde_json::to_value(FormatDocument::from_run(&run)).unwrap();
    assert_eq!(json["components"]["installed_now"][0], "demo");

    let offline = fixture(Mode::Works, true);
    let run = format(dir.path(), &default_config(), &inject(offline.host, index()));
    assert_eq!(run.checked, 0);
    assert_eq!(run.skipped[0].reason, "component not installed: demo (offline)");
    assert!(run.components.blocking());
    let json = serde_json::to_value(FormatDocument::from_run(&run)).unwrap();
    assert_eq!(json["summary"]["missing_components"], 1);
}

#[test]
fn the_bundled_build_reports_no_components_and_keeps_its_document_shape() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.py"), "x = 1\n").unwrap();
    let run = lint(dir.path(), &Config::default(), &options());
    assert!(run.components.is_empty());
    let json = serde_json::to_value(LintDocument::from_run(&run)).unwrap();
    assert!(json.get("components").is_none());
    assert!(json["summary"].get("missing_components").is_none());
    let run = format(dir.path(), &Config::default(), &options());
    let json = serde_json::to_value(FormatDocument::from_run(&run)).unwrap();
    assert!(json.get("components").is_none());
}

fn python_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), "x = 1\n").unwrap();
    dir
}

#[test]
fn a_missing_component_skips_its_files_even_where_another_engine_covers_the_language() {
    let skip = config(ComponentInstallPolicy::Auto, OnMissing::Skip);
    let reason = "component not installed: demo (offline)";
    let dir = python_project();

    let f = fixture(Mode::Works, true);
    let run = lint(dir.path(), &skip, &inject(f.host, index_of(desc_for("python"))));
    assert_eq!(
        run.checked, 0,
        "the linked quality tier must not make the file count as linted"
    );
    assert_eq!(run.skipped.len(), 1);
    assert_eq!(run.skipped[0].reason, reason);

    let f = fixture(Mode::Works, true);
    let run = format(dir.path(), &skip, &inject(f.host, index_of(desc_for("python"))));
    assert_eq!(run.checked, 0);
    assert_eq!(run.skipped.len(), 1);
    assert_eq!(run.skipped[0].reason, reason);
}

#[test]
fn the_on_missing_option_overrides_the_config_for_that_run_only() {
    let dir = project();
    let strict = default_config();

    let f = fixture(Mode::Works, true);
    let mut relaxed = inject(f.host, index());
    relaxed.on_missing = Some(OnMissing::Skip);
    assert!(!lint(dir.path(), &strict, &relaxed).components.blocking());

    let f = fixture(Mode::Works, true);
    let run = lint(dir.path(), &strict, &inject(f.host, index()));
    assert!(
        run.components.blocking(),
        "a later run without the option is back to the config"
    );
}

#[test]
fn a_component_that_describes_itself_differently_from_the_index_errors_its_files() {
    let f = fixture(Mode::Drifts, false);
    let dir = project();
    let run = lint(dir.path(), &default_config(), &inject(f.host, index()));
    assert_eq!(run.errors.len(), 1);
    assert!(
        run.errors[0].message.contains("differently from the index"),
        "{}",
        run.errors[0].message
    );
}

#[test]
fn a_malformed_index_entry_blocks_the_run_as_incompatible() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let mut broken = desc();
    broken.host_hook = "no-such-hook".into();
    let skip = config(ComponentInstallPolicy::Auto, OnMissing::Skip);
    let run = lint(dir.path(), &skip, &inject(f.host, index_of(broken)));
    assert_eq!(run.components.missing[0].kind, "incompatible");
    assert!(
        run.components.blocking(),
        "no on_missing policy turns a broken component into a skip"
    );
}

/// `--only` names an engine the run's component index describes but this build does not link,
/// as `--only sqruff` does on a core build; it must select that engine, not reject the name.
#[test]
fn only_accepts_an_engine_that_only_the_component_index_describes() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let options = RunOptions {
        only: vec!["demo".to_owned()],
        ..inject(f.host, index())
    };
    let run = lint(dir.path(), &default_config(), &options);
    assert_eq!(run.components.used, ["demo"]);
    assert_eq!(run.checked, 1);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert_eq!(run.results[0].diagnostics[0].engine, "demo");
}

#[test]
fn only_still_rejects_a_name_neither_the_build_nor_the_index_knows() {
    let f = fixture(Mode::Works, false);
    let dir = project();
    let options = RunOptions {
        only: vec!["demos".to_owned()],
        ..inject(f.host, index())
    };
    let error = poly_core::lint_run(&[dir.path().to_path_buf()], &default_config(), &options, false, false)
        .expect_err("an unknown engine name must be rejected");
    assert!(error.to_string().contains("demos"), "{error}");
    assert_eq!(f.connector.connects.load(Ordering::SeqCst), 0);
}

fn shell_desc() -> WireEngineDesc {
    let mut desc = desc_for("shell");
    desc.name = "shfmt".into();
    desc.capabilities.lint = false;
    desc
}

fn sidecar_index() -> ComponentIndex {
    let mut index = index_of(shell_desc());
    index.components[0].name = "sh".into();
    index.components[0].kind = ComponentKind::Sidecar;
    index.components[0].features = vec!["engine-sidecar-test".into()];
    index
}

fn sidecar_lock() -> String {
    lock().replace("\"component\":\"demo\"", "\"component\":\"sh\"")
}

/// Formats every shell file to a text the host `shfmt` can never produce.
struct ShFake {
    formats: AtomicUsize,
}

impl ComponentTransport for ShFake {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(encode_describe(&Describe {
            protocol: PROTOCOL_VERSION,
            component_version: "1.0.0".into(),
            engines: vec![shell_desc()],
        })?)
    }

    fn call(&self, header: &[u8], _body: &[u8]) -> anyhow::Result<Vec<u8>> {
        let payload = match decode_request(header)? {
            Request::Init { .. } => Payload::Init {
                protocol: PROTOCOL_VERSION,
                component_version: "1.0.0".into(),
            },
            Request::Format { .. } => {
                self.formats.fetch_add(1, Ordering::SeqCst);
                Payload::Formatted(WireFormatOutput::Formatted("SIDECAR\n".into()))
            }
            _ => Payload::Ok,
        };
        Ok(encode_response(&Response {
            result: Ok(payload),
            logs: Vec::new(),
        })?)
    }
}

struct ShConnector(Arc<ShFake>);

impl TransportConnector for ShConnector {
    fn connect(&self, _: &ComponentManager, _: &str) -> anyhow::Result<Arc<dyn ComponentTransport>> {
        Ok(self.0.clone())
    }
}

fn sidecar_fixture(offline: bool) -> (ComponentHost, Arc<ShFake>, tempfile::TempDir) {
    let cache = tempfile::tempdir().unwrap();
    let mut options = HostOptions::new(sidecar_lock(), PathBuf::from(cache.path()));
    options.target = Some(TARGET.into());
    options.offline = offline;
    options.gate_loads = true;
    options.sidecars = vec!["sh".into()];
    let fake = Arc::new(ShFake {
        formats: AtomicUsize::new(0),
    });
    let host = ComponentHost::new(options)
        .unwrap()
        .with_connector(Arc::new(ShConnector(fake.clone())));
    (host, fake, cache)
}

fn shell_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("run.sh"), "echo hi\n").unwrap();
    dir
}

#[test]
fn a_sidecar_formats_shell_in_place_of_the_host_tool() {
    let (host, fake, _cache) = sidecar_fixture(false);
    let dir = shell_project();
    let run = format(dir.path(), &default_config(), &inject(host, sidecar_index()));
    assert_eq!(run.components.used, ["sh"]);
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert!(fake.formats.load(Ordering::SeqCst) >= 1);
    assert!(
        run.results[0].changed,
        "the sidecar's output replaced the already-formatted source; the host shfmt would leave it"
    );
}

#[test]
fn disabling_the_sidecar_brings_the_host_tool_back() {
    let (host, fake, _cache) = sidecar_fixture(false);
    let dir = shell_project();
    let mut config = default_config();
    config.components.disable = vec!["sh".into()];
    let run = format(dir.path(), &config, &inject(host, sidecar_index()));
    assert!(run.components.is_empty(), "{:?}", run.components);
    assert_eq!(fake.formats.load(Ordering::SeqCst), 0);
    assert!(!run.results[0].changed);
}

#[test]
fn a_missing_sidecar_fails_the_run_instead_of_falling_back_to_the_host_tool() {
    let (host, fake, _cache) = sidecar_fixture(true);
    let dir = shell_project();
    let run = format(dir.path(), &default_config(), &inject(host, sidecar_index()));
    assert!(run.components.blocking(), "the CLI turns this into exit 2");
    assert_eq!(
        run.checked, 0,
        "neither the host shfmt nor the reindenter inspected the file"
    );
    assert_eq!(run.skipped[0].reason, "component not installed: sh (offline)");
    assert_eq!(fake.formats.load(Ordering::SeqCst), 0);
}

/// Indented the way the tier-2 reindenter would fix and the sidecar's fake never does.
const UNDENTED_SHELL: &str = "if true; then\necho hi\nfi\n";

#[test]
fn switching_shfmt_off_leaves_shell_alone_instead_of_reindenting_it() {
    let (host, fake, _cache) = sidecar_fixture(false);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("run.sh"), UNDENTED_SHELL).unwrap();
    let mut config = default_config();
    config.fmt = toml::from_str("[shell.shfmt]\nenabled = false\n").unwrap();
    let run = format(dir.path(), &config, &inject(host, sidecar_index()));
    assert_eq!(fake.formats.load(Ordering::SeqCst), 0, "the sidecar is not called");
    assert!(run.errors.is_empty(), "{:?}", run.errors);
    assert_eq!(
        run.skipped
            .iter()
            .map(|skip| (skip.path.file_name().and_then(|n| n.to_str()), skip.reason.as_str()))
            .collect::<Vec<_>>(),
        [(Some("run.sh"), "every engine disabled by config: [fmt.shell.shfmt]")],
        "the file is a counted skip naming the key, not silent"
    );
    assert!(poly_core::is_withdrawal_reason(&run.skipped[0].reason));
    assert!(
        run.results.iter().all(|result| !result.changed),
        "the tree-sitter reindenter does not take shell over: {:?}",
        run.results
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("run.sh")).unwrap(),
        UNDENTED_SHELL
    );
}
