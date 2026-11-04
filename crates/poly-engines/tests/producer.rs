//! The producer driven through encoded requests, as a host would drive a component.

use std::borrow::Cow;

use poly_component_protocol::{
    ConfigId, PROTOCOL_VERSION, Payload, Request, Response, WireDiagnostic, WireErrorKind, WireFile, WireFileSlice,
    WireFormatItem, WireFormatOutput, WireKind, WireLintItem, WireLogLevel, decode_describe, decode_response,
    encode_request, hooks,
};
use poly_engine_api::config::EngineConfig;
use poly_engine_api::engine::{
    BatchSupport, Capabilities, Diagnostic, Engine, FormatOutput, OptionKeys, OptionTable, OptionType, Severity,
    SourceFile, Span,
};
use poly_engine_api::language::Language;
use poly_engines::producer::{engine_config_from_wire, engine_config_to_wire};
use poly_engines::{PolyComponent, Producer};

static SQL: [Language; 1] = [Language::Sql];

/// Behaves per its name: `fake-ok` lints and formats, `fake-panic` panics, `fake-error` fails.
struct Fake(&'static str);

impl Engine for Fake {
    fn name(&self) -> &'static str {
        self.0
    }
    fn languages(&self) -> &'static [Language] {
        &SQL
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: true,
            fix: true,
            project: false,
        }
    }
    fn version(&self) -> &str {
        "9.9"
    }
    fn option_keys(&self, table: OptionTable) -> OptionKeys {
        const LINT_KEYS: &[(&str, OptionType)] = &[
            ("max", OptionType::INTEGER),
            ("mode", OptionType::STRING.or(OptionType::ARRAY)),
        ];
        match table {
            OptionTable::Lint => OptionKeys::declared(LINT_KEYS)
                .with_rule_selection()
                .with_note("a note"),
            _ => OptionKeys::UNCHECKED,
        }
    }
    fn skip_reason(&self, src: &SourceFile) -> Option<Cow<'static, str>> {
        src.content.is_empty().then_some(Cow::Borrowed("empty"))
    }
    fn batch_support(&self, _cfg: &EngineConfig) -> BatchSupport {
        BatchSupport {
            format: true,
            lint: true,
        }
    }
    fn takes_over_tier_one(&self) -> bool {
        true
    }
    fn claimed_paths(&self) -> Vec<String> {
        vec!["*.sqlx".into()]
    }
    fn lint(&self, src: &SourceFile, cfg: &EngineConfig) -> anyhow::Result<Vec<Diagnostic>> {
        match self.0 {
            "fake-panic" => panic!("boom"),
            "fake-error" => Err(anyhow::anyhow!("root cause").context("middle").context("outer")),
            _ => {
                tracing::warn!(file = %src.path.display(), "linting");
                tracing::debug!("too verbose for the default level");
                Ok(vec![Diagnostic {
                    engine: self.0.into(),
                    code: Some("F001".into()),
                    severity: Severity::Warning,
                    title: format!("{} bytes, indent {}", src.content.len(), cfg.indent_width),
                    description: Some("desc".into()),
                    span: Some(Span {
                        start_line: 1,
                        start_col: 2,
                        end_line: 3,
                        end_col: 4,
                    }),
                    url: Some("https://example.test/F001".into()),
                    fix: Vec::new(),
                    metadata: [("k".to_string(), "v".to_string())].into(),
                }])
            }
        }
    }
    fn format(&self, src: &SourceFile, _cfg: &EngineConfig) -> anyhow::Result<FormatOutput> {
        match self.0 {
            "fake-panic" => panic!("boom"),
            "fake-error" => anyhow::bail!("cannot format"),
            _ if src.content.as_ref() == "FORMATTED" => Ok(FormatOutput::Unchanged),
            _ => Ok(FormatOutput::Formatted(src.content.to_uppercase())),
        }
    }
    fn lint_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
        Ok(batch
            .iter()
            .map(|src| {
                if src.content.is_empty() {
                    Err(anyhow::anyhow!("empty member"))
                } else {
                    self.lint(src, cfg)
                }
            })
            .collect())
    }
    fn format_batch(
        &self,
        batch: &[SourceFile],
        cfg: &EngineConfig,
    ) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
        Ok(batch.iter().map(|src| self.format(src, cfg)).collect())
    }
}

fn producer() -> Producer {
    Producer::new(
        "1.2.3",
        vec![
            Box::new(Fake("fake-ok")),
            Box::new(Fake("fake-panic")),
            Box::new(Fake("fake-error")),
        ],
    )
}

fn send(producer: &Producer, request: &Request, body: &[u8]) -> Response {
    let bytes = producer.call(&encode_request(request).unwrap(), body).unwrap();
    decode_response(&bytes).unwrap()
}

fn init(producer: &Producer, level: WireLogLevel) {
    let response = send(
        producer,
        &Request::Init {
            protocol: PROTOCOL_VERSION,
            host_version: "0.0.1".into(),
            threads: 4,
            stack_size: 16 << 20,
            max_log_level: level,
        },
        b"",
    );
    assert_eq!(
        response.result,
        Ok(Payload::Init {
            protocol: PROTOCOL_VERSION,
            component_version: "1.2.3".into()
        })
    );
}

fn cfg() -> EngineConfig {
    EngineConfig {
        globals: Default::default(),
        indent_width: 2,
        options: toml::from_str("max = 3\n[nested]\nk = [1, 2]").unwrap(),
    }
}

fn register(producer: &Producer, id: ConfigId) {
    let response = send(
        producer,
        &Request::RegisterConfig {
            id,
            config: engine_config_to_wire(&cfg()),
        },
        b"",
    );
    assert_eq!(response.result, Ok(Payload::Ok));
}

fn ready() -> Producer {
    let producer = producer();
    init(&producer, WireLogLevel::Info);
    register(&producer, 1);
    producer
}

fn wire_file(len: usize) -> WireFile {
    WireFile {
        path: "q.sql".into(),
        language: "sql".into(),
        len: len as u64,
    }
}

fn direct_source(content: &str) -> SourceFile {
    SourceFile {
        path: "q.sql".into(),
        language: Language::Sql,
        content: content.into(),
    }
}

#[test]
fn lint_round_trips_diagnostics_equal_to_a_direct_call() {
    let producer = ready();
    let body = b"select 1";
    let response = send(
        &producer,
        &Request::Lint {
            engine: "fake-ok".into(),
            config: 1,
            file: wire_file(body.len()),
        },
        body,
    );
    let direct: Vec<WireDiagnostic> = Fake("fake-ok")
        .lint(&direct_source("select 1"), &cfg())
        .unwrap()
        .iter()
        .map(WireDiagnostic::from)
        .collect();
    assert_eq!(response.result, Ok(Payload::Diagnostics(direct)));
}

#[test]
fn registered_config_reaches_the_engine() {
    let wire = engine_config_to_wire(&cfg());
    let rebuilt = engine_config_from_wire(&wire).unwrap();
    assert_eq!(rebuilt.fingerprint(), cfg().fingerprint());
    let producer = ready();
    let response = send(
        &producer,
        &Request::Lint {
            engine: "fake-ok".into(),
            config: 1,
            file: wire_file(1),
        },
        b"x",
    );
    let Ok(Payload::Diagnostics(found)) = response.result else {
        panic!("expected diagnostics");
    };
    assert_eq!(found[0].title, "1 bytes, indent 2");
}

#[test]
fn format_returns_formatted_and_unchanged() {
    let producer = ready();
    let format = |body: &[u8]| {
        send(
            &producer,
            &Request::Format {
                engine: "fake-ok".into(),
                config: 1,
                file: wire_file(body.len()),
            },
            body,
        )
        .result
    };
    assert_eq!(
        format(b"abc"),
        Ok(Payload::Formatted(WireFormatOutput::Formatted("ABC".into())))
    );
    assert_eq!(
        format(b"FORMATTED"),
        Ok(Payload::Formatted(WireFormatOutput::Unchanged))
    );
}

#[test]
fn panics_are_caught_with_the_engine_guard_wording() {
    let producer = ready();
    let response = send(
        &producer,
        &Request::Lint {
            engine: "fake-panic".into(),
            config: 1,
            file: wire_file(1),
        },
        b"x",
    );
    let error = response.result.unwrap_err();
    assert_eq!(error.kind, WireErrorKind::Panic);
    assert_eq!(error.message, "the 'fake-panic' backend panicked on this file: boom");
    assert!(response.logs.iter().any(|log| log.message.contains("backend panicked")));
    // The producer stays usable after a caught panic.
    register(&producer, 2);
}

#[test]
fn errors_carry_the_full_chain() {
    let producer = ready();
    let response = send(
        &producer,
        &Request::Lint {
            engine: "fake-error".into(),
            config: 1,
            file: wire_file(1),
        },
        b"x",
    );
    let error = response.result.unwrap_err();
    assert_eq!(error.kind, WireErrorKind::Error);
    assert_eq!(error.message, "outer");
    assert_eq!(error.chain, vec!["middle".to_string(), "root cause".to_string()]);
}

#[test]
fn unknown_config_and_unknown_engine_are_distinguished() {
    let producer = ready();
    let lint = |engine: &str, config: ConfigId| {
        send(
            &producer,
            &Request::Lint {
                engine: engine.into(),
                config,
                file: wire_file(1),
            },
            b"x",
        )
        .result
        .unwrap_err()
        .kind
    };
    assert_eq!(lint("fake-ok", 99), WireErrorKind::UnknownConfig);
    assert_eq!(lint("nope", 1), WireErrorKind::UnknownEngine);
}

#[test]
fn logs_are_captured_per_call_and_filtered_by_the_init_level() {
    let producer = ready();
    let lint = || {
        send(
            &producer,
            &Request::Lint {
                engine: "fake-ok".into(),
                config: 1,
                file: wire_file(1),
            },
            b"x",
        )
    };
    let first = lint();
    assert_eq!(
        first.logs.len(),
        1,
        "the debug event is above the Info limit: {:?}",
        first.logs
    );
    assert_eq!(first.logs[0].level, WireLogLevel::Warn);
    assert!(first.logs[0].message.starts_with("linting"));
    assert!(
        first.logs[0].message.contains("file=q.sql"),
        "{:?}",
        first.logs[0].message
    );
    assert_eq!(lint().logs.len(), 1, "logs do not accumulate across calls");

    let verbose = Producer::new("1.2.3", vec![Box::new(Fake("fake-ok"))]);
    init(&verbose, WireLogLevel::Trace);
    register(&verbose, 1);
    let response = send(
        &verbose,
        &Request::Lint {
            engine: "fake-ok".into(),
            config: 1,
            file: wire_file(1),
        },
        b"x",
    );
    assert_eq!(response.logs.len(), 2);
}

#[test]
fn describe_matches_the_engines() {
    let producer = producer();
    let describe = decode_describe(&producer.describe().unwrap()).unwrap();
    assert_eq!(describe.protocol, PROTOCOL_VERSION);
    assert_eq!(describe.component_version, "1.2.3");
    let names: Vec<_> = describe.engines.iter().map(|e| e.name.as_str()).collect();
    // Sorted by name, the order the compiled component index records engines in.
    assert_eq!(names, ["fake-error", "fake-ok", "fake-panic"]);
    let engine = &describe.engines[0];
    assert_eq!(engine.languages, ["sql"]);
    assert!(
        engine.capabilities.lint
            && engine.capabilities.format
            && engine.capabilities.fix
            && !engine.capabilities.project
    );
    assert_eq!(engine.version, "9.9");
    assert!(engine.takes_over_tier_one);
    assert_eq!(engine.claimed_paths, ["*.sqlx"]);
    assert_eq!(engine.host_hook, "none");
    assert_eq!(engine.batch_shard_floor, 2);
    assert_eq!(engine.hooks & hooks::SKIP_REASON, hooks::SKIP_REASON);
    assert_eq!(engine.option_keys.len(), 3);
    let lint = &engine.option_keys[0];
    assert!(lint.checked && lint.rule_selection && !lint.has_derived);
    assert_eq!(lint.note.as_deref(), Some("a note"));
    let types: Vec<_> = lint.declared.iter().map(|k| (k.name.as_str(), k.types)).collect();
    assert_eq!(
        types,
        [
            ("max", OptionType::INTEGER.bits()),
            ("mode", OptionType::STRING.or(OptionType::ARRAY).bits())
        ]
    );
    assert!(!engine.option_keys[1].checked);
}

#[test]
fn batches_slice_the_body_and_report_per_member_failures() {
    let producer = ready();
    let body = b"abc\0def";
    let slice = |path: &str, offset: u64, len: u64| WireFileSlice {
        path: path.into(),
        language: "sql".into(),
        offset,
        len,
    };
    let files = vec![slice("a.sql", 0, 3), slice("empty.sql", 3, 0), slice("b.sql", 4, 3)];
    let response = send(
        &producer,
        &Request::LintBatch {
            engine: "fake-ok".into(),
            config: 1,
            files: files.clone(),
        },
        body,
    );
    let Ok(Payload::LintBatch(items)) = response.result else {
        panic!("expected a lint batch");
    };
    assert!(matches!(&items[0], WireLintItem::Ok(d) if d[0].title == "3 bytes, indent 2"));
    assert!(matches!(&items[1], WireLintItem::Err(e) if e.message == "empty member"));
    assert!(matches!(&items[2], WireLintItem::Ok(_)));

    let response = send(
        &producer,
        &Request::FormatBatch {
            engine: "fake-ok".into(),
            config: 1,
            files,
        },
        body,
    );
    let Ok(Payload::FormatBatch(items)) = response.result else {
        panic!("expected a format batch");
    };
    assert_eq!(items[0], WireFormatItem::Ok(WireFormatOutput::Formatted("ABC".into())));

    let out_of_range = send(
        &producer,
        &Request::LintBatch {
            engine: "fake-ok".into(),
            config: 1,
            files: vec![slice("x.sql", 5, 9)],
        },
        body,
    );
    assert_eq!(out_of_range.result.unwrap_err().kind, WireErrorKind::Protocol);
}

#[test]
fn hooks_and_predicates_dispatch_to_the_engine() {
    let producer = ready();
    let skip = |body: &[u8]| {
        send(
            &producer,
            &Request::SkipReason {
                engine: "fake-ok".into(),
                file: wire_file(body.len()),
            },
            body,
        )
        .result
    };
    assert_eq!(skip(b""), Ok(Payload::OptString(Some("empty".into()))));
    assert_eq!(skip(b"x"), Ok(Payload::OptString(None)));
    let provides = send(
        &producer,
        &Request::ProvidesLanguage {
            engine: "fake-ok".into(),
            config: 1,
            language: "sql".into(),
            kind: WireKind::Lint,
        },
        b"",
    );
    assert_eq!(provides.result, Ok(Payload::Bool(true)));
    let support = send(
        &producer,
        &Request::BatchSupport {
            engine: "fake-ok".into(),
            config: 1,
        },
        b"",
    );
    assert!(matches!(support.result, Ok(Payload::BatchSupport(s)) if s.format && s.lint));
    let run = send(
        &producer,
        &Request::RunContext {
            engine: "fake-ok".into(),
            config: 1,
        },
        b"",
    );
    assert_eq!(run.result, Ok(Payload::Text(String::new())));
}

#[test]
fn protocol_rules_are_enforced() {
    let fresh = producer();
    let before_init = send(
        &fresh,
        &Request::BatchSupport {
            engine: "fake-ok".into(),
            config: 1,
        },
        b"",
    );
    assert_eq!(before_init.result.unwrap_err().kind, WireErrorKind::Protocol);

    let wrong = send(
        &fresh,
        &Request::Init {
            protocol: PROTOCOL_VERSION + 1,
            host_version: "x".into(),
            threads: 1,
            stack_size: 1,
            max_log_level: WireLogLevel::Info,
        },
        b"",
    );
    assert_eq!(wrong.result.unwrap_err().kind, WireErrorKind::Protocol);

    let ready = ready();
    let again = send(
        &ready,
        &Request::Init {
            protocol: PROTOCOL_VERSION,
            host_version: "x".into(),
            threads: 1,
            stack_size: 1,
            max_log_level: WireLogLevel::Info,
        },
        b"",
    );
    assert_eq!(again.result.unwrap_err().kind, WireErrorKind::Protocol);
    assert_eq!(ready.init_state().unwrap().threads, 4);

    let garbage = decode_response(&ready.call(b"\xc1not msgpack", b"").unwrap()).unwrap();
    assert_eq!(garbage.result.unwrap_err().kind, WireErrorKind::Protocol);

    let mismatched = send(
        &ready,
        &Request::Lint {
            engine: "fake-ok".into(),
            config: 1,
            file: wire_file(5),
        },
        b"x",
    );
    assert_eq!(mismatched.result.unwrap_err().kind, WireErrorKind::Protocol);
}

#[cfg(feature = "sql")]
#[test]
fn sqruff_through_the_producer_matches_a_direct_call() {
    use poly_engines::sql::SqruffEngine;

    // Non-layout findings (AL02, CV05): sqruff's layout rules are dropped because `poly fmt` owns them.
    let sql = "select a b, id,name from users where id = null\n";
    let config = EngineConfig {
        globals: Default::default(),
        indent_width: Language::Sql.default_indent_width(),
        options: toml::Table::new(),
    };
    let direct: Vec<WireDiagnostic> = SqruffEngine
        .lint(&direct_source(sql), &config)
        .unwrap()
        .iter()
        .map(WireDiagnostic::from)
        .collect();
    assert!(!direct.is_empty(), "the fixture must produce findings");

    let producer = Producer::new("1.2.3", vec![Box::new(SqruffEngine)]);
    init(&producer, WireLogLevel::Info);
    let registered = send(
        &producer,
        &Request::RegisterConfig {
            id: 1,
            config: engine_config_to_wire(&config),
        },
        b"",
    );
    assert_eq!(registered.result, Ok(Payload::Ok));
    let response = send(
        &producer,
        &Request::Lint {
            engine: SqruffEngine.name().into(),
            config: 1,
            file: wire_file(sql.len()),
        },
        sql.as_bytes(),
    );
    assert_eq!(response.result, Ok(Payload::Diagnostics(direct)));
}
