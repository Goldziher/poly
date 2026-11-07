//! An in-process component speaking the protocol for one engine, `demo`.
#![allow(dead_code)] // each test binary uses a different subset

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use poly_component_host::{ComponentTransport, InitParams};
use poly_component_protocol::{
    Describe, PROTOCOL_VERSION, Payload, Request, Response, WireCapabilities, WireDiagnostic, WireEngineDesc,
    WireError, WireErrorKind, WireFormatOutput, WireLog, WireLogLevel, WireSeverity, decode_request, encode_describe,
    encode_response,
};
use poly_engine_api::config::{EngineConfig, GlobalDefaults};
use poly_engine_api::engine::SourceFile;
use poly_engine_api::language::Language;

pub fn desc(hooks: u32, host_hook: &str) -> WireEngineDesc {
    WireEngineDesc {
        name: "demo".into(),
        languages: vec!["sql".into()],
        capabilities: WireCapabilities {
            lint: true,
            format: true,
            fix: false,
            project: false,
        },
        version: "1.2.3".into(),
        option_keys: Vec::new(),
        hooks,
        takes_over_tier_one: false,
        claimed_paths: Vec::new(),
        host_hook: host_hook.into(),
        supplements_tier_one: false,
        self_manages_enabled: false,
        supersedes_generic_formatter: false,
        batch_shard_floor: 2,
        project_inputs: Default::default(),
    }
}

pub fn describe_of(engine: WireEngineDesc) -> Describe {
    Describe {
        protocol: PROTOCOL_VERSION,
        component_version: "9.9.9".into(),
        engines: vec![engine],
    }
}

pub fn source(text: &str) -> SourceFile {
    SourceFile {
        path: PathBuf::from("q.sql"),
        language: Language::Sql,
        content: Arc::from(text),
    }
}

pub fn config(indent: usize) -> EngineConfig {
    EngineConfig {
        globals: GlobalDefaults::default(),
        indent_width: indent,
        options: toml::Table::new(),
    }
}

pub fn init() -> InitParams {
    InitParams::default()
}

#[derive(Default)]
struct State {
    calls: Vec<&'static str>,
    registered: HashSet<u32>,
}

/// Behaviour by body: `panic` panics, `error` fails with a chain, anything else lints to one
/// diagnostic titled with the body length and formats to upper case.
pub struct FakeComponent {
    describe: Describe,
    state: Mutex<State>,
}

impl FakeComponent {
    pub fn new(describe: Describe) -> Arc<Self> {
        Arc::new(Self {
            describe,
            state: Mutex::new(State::default()),
        })
    }

    pub fn calls(&self) -> Vec<&'static str> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn count(&self, name: &str) -> usize {
        self.calls().iter().filter(|c| **c == name).count()
    }

    /// Simulate the component forgetting its registered configs.
    pub fn forget_configs(&self) {
        self.state.lock().unwrap().registered.clear();
    }

    fn answer(&self, request: &Request, body: &[u8]) -> Response {
        let mut state = self.state.lock().unwrap();
        state.calls.push(request.variant_name());
        let ok = |payload| Response {
            result: Ok(payload),
            logs: Vec::new(),
        };
        let fail = |kind, message: &str, chain: &[&str]| Response {
            result: Err(WireError {
                kind,
                message: message.into(),
                chain: chain.iter().map(|c| (*c).to_owned()).collect(),
            }),
            logs: Vec::new(),
        };
        match request {
            Request::Init { .. } => ok(Payload::Init {
                protocol: PROTOCOL_VERSION,
                component_version: "9.9.9".into(),
            }),
            Request::RegisterConfig { id, .. } => {
                state.registered.insert(*id);
                ok(Payload::Ok)
            }
            Request::Lint { config, file, .. } | Request::Format { config, file, .. } => {
                if !state.registered.contains(config) {
                    return fail(WireErrorKind::UnknownConfig, "no such config", &[]);
                }
                assert_eq!(file.len as usize, body.len());
                if body == b"panic" {
                    return fail(
                        WireErrorKind::Panic,
                        "the 'demo' backend panicked on this file: index out of bounds",
                        &[],
                    );
                }
                if body == b"error" {
                    return fail(WireErrorKind::Error, "parse failed", &["bad token", "at byte 3"]);
                }
                let logs = vec![WireLog {
                    level: WireLogLevel::Warn,
                    target: "demo::parser".into(),
                    message: "recovered from odd input".into(),
                }];
                let payload = if matches!(request, Request::Lint { .. }) {
                    Payload::Diagnostics(vec![WireDiagnostic {
                        engine: "demo".into(),
                        code: Some("D001".into()),
                        severity: WireSeverity::Warning,
                        title: format!("{} bytes", body.len()),
                        description: None,
                        span: None,
                        url: None,
                        fix: Vec::new(),
                        metadata: Default::default(),
                    }])
                } else {
                    let text = String::from_utf8_lossy(body).to_uppercase();
                    if text.as_bytes() == body {
                        Payload::Formatted(WireFormatOutput::Unchanged)
                    } else {
                        Payload::Formatted(WireFormatOutput::Formatted(text))
                    }
                };
                Response {
                    result: Ok(payload),
                    logs,
                }
            }
            Request::SkipReason { .. } => ok(Payload::OptString(Some("generated file".into()))),
            Request::ProvidesLanguage { language, .. } => ok(Payload::Bool(language == "sql")),
            other => fail(WireErrorKind::Protocol, other.variant_name(), &[]),
        }
    }
}

impl ComponentTransport for FakeComponent {
    fn describe(&self) -> anyhow::Result<Vec<u8>> {
        Ok(encode_describe(&self.describe)?)
    }

    fn call(&self, header: &[u8], body: &[u8]) -> anyhow::Result<Vec<u8>> {
        let request = decode_request(header)?;
        Ok(encode_response(&self.answer(&request, body))?)
    }
}
