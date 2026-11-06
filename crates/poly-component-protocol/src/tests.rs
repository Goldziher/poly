use std::collections::BTreeMap;

use proptest::prelude::*;

use super::*;

fn arb_toml() -> impl Strategy<Value = toml::Value> {
    let datetime = prop::sample::select(vec![
        "1979-05-27T07:32:00Z",
        "1979-05-27T00:32:00.999999-07:00",
        "1979-05-27",
        "07:32:00",
        "1979-05-27T07:32:00",
    ])
    .prop_map(|s| toml::Value::Datetime(s.parse().unwrap()));
    let leaf = prop_oneof![
        any::<bool>().prop_map(toml::Value::Boolean),
        any::<i64>().prop_map(toml::Value::Integer),
        (-1.0e12f64..1.0e12f64).prop_map(toml::Value::Float),
        any::<String>().prop_map(toml::Value::String),
        datetime,
    ];
    leaf.prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(toml::Value::Array),
            prop::collection::btree_map(any::<String>(), inner, 0..5).prop_map(|m| {
                let mut table = toml::Table::new();
                for (k, v) in m {
                    table.insert(k, v);
                }
                toml::Value::Table(table)
            }),
        ]
    })
}

proptest! {
    #[test]
    fn toml_value_round_trips_through_wire_value(value in arb_toml()) {
        let wire = WireValue::from(&value);
        prop_assert_eq!(to_toml(&wire).unwrap(), value);
    }

    #[test]
    fn wire_value_round_trips_through_messagepack(value in arb_toml()) {
        let request = Request::RegisterConfig { id: 7, config: WireValue::from(value.clone()) };
        let decoded = decode_request(&encode_request(&request).unwrap()).unwrap();
        prop_assert_eq!(&decoded, &request);
        let Request::RegisterConfig { config, .. } = decoded else { unreachable!() };
        prop_assert_eq!(to_toml(&config).unwrap(), value);
    }
}

#[test]
fn table_key_order_is_preserved_on_the_wire() {
    let wire = WireValue::Table(vec![
        ("z".into(), WireValue::Integer(1)),
        ("a".into(), WireValue::Integer(2)),
    ]);
    let request = Request::RegisterConfig {
        id: 1,
        config: wire.clone(),
    };
    let Request::RegisterConfig { config, .. } = decode_request(&encode_request(&request).unwrap()).unwrap() else {
        panic!("wrong variant");
    };
    assert_eq!(config, wire);
}

#[test]
fn invalid_wire_values_are_rejected() {
    assert_eq!(
        to_toml(&WireValue::Datetime("nope".into())),
        Err(WireValueError::InvalidDatetime("nope".into()))
    );
    let dup = WireValue::Table(vec![
        ("k".into(), WireValue::Boolean(true)),
        ("k".into(), WireValue::Boolean(false)),
    ]);
    assert_eq!(to_toml(&dup), Err(WireValueError::DuplicateKey("k".into())));
}

fn file() -> WireFile {
    WireFile {
        path: "a/b.py".into(),
        language: "python".into(),
        len: 12,
    }
}

fn slice(offset: u64) -> WireFileSlice {
    WireFileSlice {
        path: "a/b.py".into(),
        language: "python".into(),
        offset,
        len: 5,
    }
}

fn diagnostic(full: bool) -> WireDiagnostic {
    WireDiagnostic {
        engine: "ruff".into(),
        code: full.then(|| "E501".to_string()),
        severity: WireSeverity::Warning,
        title: "line too long".into(),
        description: full.then(|| "details".to_string()),
        span: full.then_some(WireSpan {
            start_line: 1,
            start_col: 2,
            end_line: 3,
            end_col: 4,
        }),
        url: full.then(|| "https://example.test".to_string()),
        fix: if full {
            vec![WireEdit {
                start_byte: 0,
                end_byte: 4,
                replacement: "x".into(),
            }]
        } else {
            vec![]
        },
        metadata: if full {
            BTreeMap::from([("k".to_string(), "v".to_string())])
        } else {
            BTreeMap::new()
        },
    }
}

fn error() -> WireError {
    WireError {
        kind: WireErrorKind::Panic,
        message: "boom".into(),
        chain: vec!["a".into(), "b".into()],
    }
}

fn rules_report(with_tests: bool) -> WireRulesReport {
    WireRulesReport {
        builtin_pack_enabled: true,
        rules: vec![WireRule {
            id: "swallowed-error".into(),
            language: "rust".into(),
            source: "builtin".into(),
            default_severity: "warning".into(),
            severity: "warning".into(),
            enabled: true,
        }],
        tests: with_tests.then(|| WireRuleTests {
            total_rules: 1,
            outcomes: vec![WireRuleTestOutcome {
                rule_id: "swallowed-error".into(),
                kind: "invalid".into(),
                index: 0,
                passed: false,
                detail: Some("no match".into()),
            }],
            missing_rule_ids: vec!["ghost".into()],
            untested_rule_ids: vec![],
        }),
    }
}

fn sample_requests() -> Vec<Request> {
    let cfg = WireValue::Table(vec![("indent_width".into(), WireValue::Integer(4))]);
    vec![
        Request::Init {
            protocol: PROTOCOL_VERSION,
            host_version: "0.1.0".into(),
            threads: 8,
            stack_size: 16 << 20,
            max_log_level: WireLogLevel::Debug,
        },
        Request::RegisterConfig {
            id: 3,
            config: cfg.clone(),
        },
        Request::Lint {
            engine: "ruff".into(),
            config: 3,
            file: file(),
        },
        Request::Format {
            engine: "ruff".into(),
            config: 3,
            file: file(),
        },
        Request::LintBatch {
            engine: "e".into(),
            config: 3,
            files: vec![slice(0), slice(5)],
        },
        Request::FormatBatch {
            engine: "e".into(),
            config: 3,
            files: vec![slice(0)],
        },
        Request::ProjectCheck {
            engine: "ty".into(),
            root: "/r".into(),
            files: vec![slice(0)],
            config: 3,
            environment: vec![("K".into(), "V".into())],
        },
        Request::SkipReason {
            engine: "yaml".into(),
            file: file(),
        },
        Request::LintCacheSalt {
            engine: "helm".into(),
            file: file(),
        },
        Request::ProvidesLanguage {
            engine: "e".into(),
            config: 3,
            language: "go".into(),
            kind: WireKind::Format,
        },
        Request::BatchSupport {
            engine: "e".into(),
            config: 3,
        },
        Request::DerivedOptionKeys {
            engine: "e".into(),
            table: WireOptionTable::CrossCuttingLint,
            options: cfg,
        },
        Request::RunContext {
            engine: "e".into(),
            config: 3,
        },
        Request::FileContext {
            engine: "e".into(),
            config: 3,
            path: "p".into(),
        },
        Request::LintInputSalt {
            engine: "e".into(),
            config: 3,
            path: "p".into(),
        },
        Request::FormatInputSalt {
            engine: "e".into(),
            config: 3,
            path: "p".into(),
        },
        Request::Rules {
            dirs: vec![".poly/rules".into()],
            config: WireValue::Table(vec![("options".into(), WireValue::Table(vec![]))]),
            test: true,
        },
    ]
}

fn sample_payloads() -> Vec<Payload> {
    vec![
        Payload::Ok,
        Payload::Init {
            protocol: 1,
            component_version: "1".into(),
        },
        Payload::Diagnostics(vec![diagnostic(true), diagnostic(false)]),
        Payload::Formatted(WireFormatOutput::Unchanged),
        Payload::Formatted(WireFormatOutput::Formatted("x\n".into())),
        Payload::LintBatch(vec![
            WireLintItem::Ok(vec![diagnostic(true)]),
            WireLintItem::Err(error()),
        ]),
        Payload::FormatBatch(vec![
            WireFormatItem::Ok(WireFormatOutput::Unchanged),
            WireFormatItem::Err(error()),
        ]),
        Payload::Project(vec![WireFileDiagnostics {
            path: "a".into(),
            diagnostics: vec![diagnostic(false)],
        }]),
        Payload::Bool(true),
        Payload::OptString(None),
        Payload::OptString(Some("salt".into())),
        Payload::Text("ctx".into()),
        Payload::BatchSupport(WireBatchSupport {
            format: true,
            lint: false,
        }),
        Payload::OptionKeys(None),
        Payload::OptionKeys(Some(vec!["a".into()])),
        Payload::Rules(rules_report(true)),
        Payload::Rules(rules_report(false)),
    ]
}

#[test]
fn every_request_variant_round_trips() {
    let requests = sample_requests();
    let names: Vec<&str> = requests.iter().map(Request::variant_name).collect();
    assert_eq!(
        names, REQUEST_VARIANTS,
        "samples must cover every variant in declaration order"
    );
    for request in requests {
        let bytes = encode_request(&request).unwrap();
        assert_eq!(decode_request(&bytes).unwrap(), request);
    }
}

#[test]
fn every_response_variant_round_trips() {
    let logs = vec![WireLog {
        level: WireLogLevel::Warn,
        target: "t".into(),
        message: "m".into(),
    }];
    let mut responses: Vec<Response> = sample_payloads()
        .into_iter()
        .map(|p| Response {
            result: Ok(p),
            logs: logs.clone(),
        })
        .collect();
    for kind in [
        WireErrorKind::Error,
        WireErrorKind::Panic,
        WireErrorKind::UnknownConfig,
        WireErrorKind::UnknownEngine,
        WireErrorKind::Protocol,
        WireErrorKind::Unsupported,
    ] {
        responses.push(Response {
            result: Err(WireError { kind, ..error() }),
            logs: vec![],
        });
    }
    for response in responses {
        let bytes = encode_response(&response).unwrap();
        assert_eq!(decode_response(&bytes).unwrap(), response);
    }
}

#[test]
fn describe_round_trips() {
    let describe = Describe {
        protocol: PROTOCOL_VERSION,
        component_version: "1".into(),
        engines: vec![WireEngineDesc {
            name: "ruff".into(),
            languages: vec!["python".into()],
            capabilities: WireCapabilities {
                lint: true,
                format: true,
                fix: true,
                project: false,
            },
            version: "0.17".into(),
            option_keys: vec![WireOptionKeys {
                table: WireOptionTable::Lint,
                checked: true,
                declared: vec![WireOptionKey {
                    name: "select".into(),
                    types: 16,
                }],
                rule_selection: true,
                has_derived: false,
                note: Some("n".into()),
            }],
            hooks: hooks::RUN_CONTEXT | hooks::SKIP_REASON,
            takes_over_tier_one: false,
            claimed_paths: vec![],
            host_hook: "none".into(),
            supplements_tier_one: false,
            self_manages_enabled: false,
            supersedes_generic_formatter: true,
            batch_shard_floor: 2,
            project_inputs: Default::default(),
        }],
    };
    assert_eq!(decode_describe(&encode_describe(&describe).unwrap()).unwrap(), describe);
}

#[test]
fn header_uses_named_fields() {
    let bytes = encode_request(&Request::BatchSupport {
        engine: "e".into(),
        config: 1,
    })
    .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("engine") && text.contains("config"), "{text:?}");
}

#[test]
fn trailing_bytes_are_rejected() {
    let mut bytes = encode_request(&sample_requests()[1]).unwrap();
    bytes.push(0);
    assert!(matches!(
        decode_request(&bytes),
        Err(ProtocolError::TrailingBytes { extra: 1 })
    ));
}

/// Deterministic xorshift so failures reproduce.
fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[test]
fn garbage_never_decodes_and_never_panics() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut buffers: Vec<Vec<u8>> = vec![vec![]];
    for byte in 0..=255u8 {
        for len in [1usize, 2, 8, 64] {
            buffers.push(vec![byte; len]);
        }
    }
    for _ in 0..4000 {
        let len = (xorshift(&mut state) % 96) as usize;
        buffers.push((0..len).map(|_| xorshift(&mut state) as u8).collect());
    }
    for buf in &buffers {
        assert!(decode_request(buf).is_err(), "request {buf:?}");
        assert!(decode_response(buf).is_err(), "response {buf:?}");
        assert!(decode_describe(buf).is_err(), "describe {buf:?}");
    }
}

#[test]
fn every_truncation_of_a_valid_message_is_an_error() {
    for request in sample_requests() {
        let bytes = encode_request(&request).unwrap();
        for cut in 0..bytes.len() {
            assert!(
                decode_request(&bytes[..cut]).is_err(),
                "{} cut at {cut}",
                request.variant_name()
            );
        }
    }
    for payload in sample_payloads() {
        let bytes = encode_response(&Response {
            result: Ok(payload),
            logs: vec![],
        })
        .unwrap();
        for cut in 0..bytes.len() {
            assert!(decode_response(&bytes[..cut]).is_err(), "cut at {cut}");
        }
    }
}

#[test]
fn single_byte_corruption_never_panics() {
    for request in sample_requests() {
        let bytes = encode_request(&request).unwrap();
        for i in 0..bytes.len() {
            for flip in [0x00u8, 0xff, 0xc1, 0xdd, 0xdf] {
                let mut mutated = bytes.clone();
                mutated[i] = flip;
                let _ = decode_request(&mutated);
            }
        }
    }
}

#[test]
fn coverage_table_references_real_requests_and_has_no_duplicates() {
    let mut seen = std::collections::BTreeSet::new();
    for (method, coverage) in ENGINE_METHODS {
        assert!(seen.insert(*method), "duplicate method {method}");
        if let Coverage::Request(variant) = coverage {
            assert!(
                REQUEST_VARIANTS.contains(variant),
                "{method} -> unknown request {variant}"
            );
        }
    }
}

#[test]
fn committed_schema_matches_generated() {
    let generated = json_schema();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../schemas/component-protocol.json");
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(path, &generated).unwrap();
        return;
    }
    // Compared as JSON values: `poly fmt` owns the committed file's layout.
    let committed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let generated: serde_json::Value = serde_json::from_str(&generated).unwrap();
    assert!(
        committed == generated,
        "schemas/component-protocol.json is stale; regenerate with `UPDATE_SCHEMA=1 cargo test -p poly-component-protocol committed_schema`"
    );
}
