use poly_cache::{Namespace, ResultCache};

use super::*;
use crate::config::{Config, Kind};
use crate::engine::{Capabilities, Engine};
use crate::filter::SeverityRemap;

struct Stub {
    version: &'static str,
    run_context: &'static str,
}

impl Engine for Stub {
    fn name(&self) -> &'static str {
        "stub"
    }

    fn languages(&self) -> &'static [Language] {
        &[]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            lint: true,
            format: false,
            fix: false,
        }
    }

    fn version(&self) -> &str {
        self.version
    }

    fn run_context(&self, _cfg: &crate::config::EngineConfig) -> String {
        self.run_context.to_owned()
    }
}

fn engine_plan(stub: Stub, args: &toml::Table) -> EnginePlan {
    let config = Config::default().engine_config(&Language::Python, "stub", Kind::Lint);
    let mut args = args.clone();
    let context = stub.run_context;
    EnginePlan {
        engine: Box::new(stub),
        config,
        serialized_args: {
            if !context.is_empty() {
                args.insert("__run_context".to_owned(), toml::Value::String(context.to_owned()));
            }
            ResultCache::serialize_args(&args)
        },
        severity_remap: SeverityRemap::new(Vec::new()),
        provides_language_lint: true,
        format_ignore: None,
    }
}

fn key(stub: Stub, args: &toml::Table) -> PlanKey {
    base_key(Namespace::Lint, &engine_plan(stub, args))
}

const BASE: Stub = Stub {
    version: "1",
    run_context: "",
};

#[test]
fn an_engine_version_bump_changes_the_key() {
    let args = toml::Table::new();
    assert_eq!(key(BASE, &args), key(BASE, &args));
    assert_ne!(key(BASE, &args), key(Stub { version: "2", ..BASE }, &args));
}

#[test]
fn an_engine_option_change_changes_the_key() {
    let mut other = toml::Table::new();
    other.insert("select".to_owned(), toml::Value::String("F401".to_owned()));
    assert_ne!(key(BASE, &toml::Table::new()), key(BASE, &other));
}

#[test]
fn a_change_in_the_engines_run_context_changes_the_key() {
    let args = toml::Table::new();
    assert_ne!(
        key(BASE, &args),
        key(
            Stub {
                run_context: "module tree v2",
                ..BASE
            },
            &args
        )
    );
}

#[test]
fn the_namespace_is_part_of_the_key() {
    let plan = engine_plan(BASE, &toml::Table::new());
    assert_ne!(base_key(Namespace::Lint, &plan), base_key(Namespace::Fmt, &plan));
}

#[test]
fn an_empty_payload_means_nothing_found_and_findings_round_trip() {
    assert!(decode_payload(&[]).is_some_and(|found| found.is_empty()));
    assert_eq!(encode_payload(&[]), Vec::<u8>::new());
    let finding = Diagnostic {
        engine: "ruff".to_owned(),
        code: Some("F401".to_owned()),
        severity: crate::engine::Severity::Warning,
        title: "unused".to_owned(),
        description: None,
        span: None,
        url: None,
        fix: Vec::new(),
        metadata: Default::default(),
    };
    let round_tripped = decode_payload(&encode_payload(std::slice::from_ref(&finding))).unwrap();
    assert_eq!(round_tripped.len(), 1);
    assert_eq!(round_tripped[0].code, finding.code);
    assert!(decode_payload(b"not json").is_none());
}
