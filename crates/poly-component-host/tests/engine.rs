//! `ComponentEngine` against an in-process fake component.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{FakeComponent, config, desc, describe_of, init, source};
use poly_component_host::{ComponentEngine, ComponentSlot, SlotStatus, component_version};
use poly_component_protocol::hooks;
use poly_engine_api::engine::{BatchSupport, Engine, FormatOutput};

fn engine_with(hooks: u32, host_hook: &str) -> (ComponentEngine, Arc<FakeComponent>) {
    let wire = desc(hooks, host_hook);
    let fake = FakeComponent::new(describe_of(wire.clone()));
    let slot = ComponentSlot::from_transport("demo-component", Some(describe_of(wire.clone())), init(), fake.clone());
    let engine = ComponentEngine::new("demo-component", &wire, "0123456789abcdef0123456789abcdef", slot).unwrap();
    (engine, fake)
}

#[test]
fn lint_and_format_round_trip() {
    let (engine, _) = engine_with(0, "none");
    let found = engine.lint(&source("select 1"), &config(4)).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code.as_deref(), Some("D001"));
    assert_eq!(found[0].title, "8 bytes");
    match engine.format(&source("select 1"), &config(4)).unwrap() {
        FormatOutput::Formatted(text) => assert_eq!(text, "SELECT 1"),
        FormatOutput::Unchanged => panic!("expected a rewrite"),
    }
    assert!(matches!(
        engine.format(&source("SELECT 1"), &config(4)).unwrap(),
        FormatOutput::Unchanged
    ));
}

#[test]
fn config_is_registered_once_per_fingerprint() {
    let (engine, fake) = engine_with(0, "none");
    for _ in 0..3 {
        engine.lint(&source("a"), &config(4)).unwrap();
    }
    assert_eq!(fake.count("RegisterConfig"), 1);
    engine.lint(&source("a"), &config(2)).unwrap();
    assert_eq!(fake.count("RegisterConfig"), 2);
    assert_eq!(fake.count("Init"), 1);
}

#[test]
fn unknown_config_is_registered_again_and_retried() {
    let (engine, fake) = engine_with(0, "none");
    engine.lint(&source("a"), &config(4)).unwrap();
    fake.forget_configs();
    let found = engine.lint(&source("a"), &config(4)).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(fake.count("RegisterConfig"), 2);
    assert_eq!(fake.count("Lint"), 3);
}

#[test]
fn panic_reads_like_the_engine_guard() {
    let (engine, _) = engine_with(0, "none");
    let error = engine.lint(&source("panic"), &config(4)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "the 'demo' backend panicked on this file: index out of bounds"
    );
}

#[test]
fn error_keeps_message_and_chain() {
    let (engine, _) = engine_with(0, "none");
    let error = engine.format(&source("error"), &config(4)).unwrap_err();
    assert_eq!(error.to_string(), "parse failed");
    let chain: Vec<String> = error.chain().map(ToString::to_string).collect();
    assert_eq!(chain, ["parse failed", "bad token", "at byte 3"]);
}

#[test]
fn cache_path_hooks_never_reach_the_component() {
    let all = hooks::RUN_CONTEXT | hooks::FILE_CONTEXT | hooks::LINT_INPUT_SALT | hooks::FORMAT_INPUT_SALT;
    let (engine, fake) = engine_with(all, "none");
    let cfg = config(4);
    let path = Path::new("q.sql");
    assert_eq!(engine.run_context(&cfg), "");
    assert_eq!(engine.file_context(path, &cfg), None);
    assert_eq!(engine.lint_input_salt(path, &cfg), None);
    assert_eq!(engine.format_input_salt(path, &cfg), None);
    let src = source("x");
    assert!(matches!(
        engine.format_config(&src, &cfg),
        std::borrow::Cow::Borrowed(_)
    ));
    assert_eq!(engine.name(), "demo");
    assert!(engine.capabilities().lint);
    assert!(engine.provides_language_lint(&src.language, &cfg));
    assert!(engine.skip_reason(&src).is_none());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(engine.slot().status(), SlotStatus::Unloaded);
}

#[test]
fn per_file_hooks_are_called_only_when_the_bit_is_set() {
    let (engine, fake) = engine_with(hooks::SKIP_REASON | hooks::PROVIDES_LANGUAGE, "none");
    let src = source("x");
    let cfg = config(4);
    assert_eq!(engine.skip_reason(&src).as_deref(), Some("generated file"));
    assert!(engine.provides_language_lint(&src.language, &cfg));
    assert!(!engine.provides_language_lint(&poly_engine_api::language::Language::Go, &cfg));
    assert_eq!(fake.count("SkipReason"), 1);
    assert_eq!(fake.count("ProvidesLanguage"), 2);
}

#[test]
fn undeclared_per_file_hooks_answer_the_default_without_connecting() {
    let (engine, fake) = engine_with(0, "none");
    let src = source("x");
    let cfg = config(4);
    assert_eq!(engine.lint_cache_salt(&src), None);
    assert_eq!(engine.lint_cache_version(&src), engine.version());
    assert_eq!(engine.batch_support(&cfg), BatchSupport::default());
    assert!(engine.provides_language_lint(&src.language, &cfg));
    assert!(engine.provides_language_format(&src.language, &cfg));
    assert!(engine.provides_language_lint(&poly_engine_api::language::Language::Go, &cfg));
    assert!(engine.skip_reason(&src).is_none());
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(engine.slot().status(), SlotStatus::Unloaded);
}

#[test]
fn unknown_host_hook_is_rejected() {
    let wire = desc(0, "bogus");
    let slot = ComponentSlot::failed("demo-component", "x");
    assert!(ComponentEngine::new("demo-component", &wire, "ab", slot).is_err());
}

#[test]
fn version_embeds_the_first_sixteen_lock_digits() {
    let (engine, _) = engine_with(0, "none");
    assert_eq!(engine.version(), "1.2.3+c0123456789abcdef");
    assert_eq!(component_version("1.2.3", "abcd"), "1.2.3+cabcd");
    assert_ne!(component_version("1.2.3", "aa".repeat(32).as_str()), engine.version());
}

#[test]
fn static_metadata_comes_from_the_index() {
    let (engine, fake) = engine_with(0, "oxfmt-scope");
    assert_eq!(engine.languages().len(), 1);
    assert_eq!(engine.languages()[0].id(), "sql");
    assert_eq!(engine.batch_shard_floor(), 2);
    assert_eq!(engine.host_hook(), poly_engine_api::host_hooks::HostHook::OxfmtScope);
    assert!(fake.calls().is_empty());
}

#[test]
fn unavailable_engine_carries_its_reason() {
    let wire = desc(0, "none");
    let engine =
        ComponentEngine::unavailable("demo-component", &wire, "feedfacefeedface00", "offline and not cached").unwrap();
    let src = source("x");
    let cfg = config(4);
    assert_eq!(engine.unavailable_reason(), Some("offline and not cached"));
    assert_eq!(engine.skip_reason(&src).as_deref(), Some("offline and not cached"));
    assert!(!engine.provides_language_lint(&src.language, &cfg));
    assert_eq!(engine.version(), "1.2.3+cfeedfacefeedface+unavailable");
    let error = engine.lint(&src, &cfg).unwrap_err();
    assert!(error.to_string().contains("offline and not cached"), "{error:#}");
}

#[test]
fn describe_mismatch_fails_the_load_once() {
    let wire = desc(0, "none");
    let mut other = wire.clone();
    other.version = "9.9.9".into();
    let fake = FakeComponent::new(describe_of(other));
    let slot = ComponentSlot::from_transport("demo-component", Some(describe_of(wire.clone())), init(), fake.clone());
    let engine = ComponentEngine::new("demo-component", &wire, "ab", slot).unwrap();
    let first = engine.lint(&source("x"), &config(4)).unwrap_err().to_string();
    assert!(first.contains("differently from the index"), "{first}");
    let second = engine.lint(&source("x"), &config(4)).unwrap_err().to_string();
    assert_eq!(first, second);
    assert_eq!(fake.count("Init"), 1);
    assert!(matches!(engine.slot().status(), SlotStatus::Failed(_)));
}

#[test]
fn failing_stub_declines_nothing_and_fails_every_call() {
    let wire = desc(0, "none");
    let engine = ComponentEngine::unavailable_failing("demo-component", &wire, "feedfacefeedface00", "boom").unwrap();
    let src = source("x");
    assert_eq!(engine.skip_reason(&src), None);
    assert_eq!(engine.unavailable_reason(), Some("boom"));
    let error = engine.lint(&src, &config(4)).unwrap_err();
    assert!(error.to_string().contains("boom"), "{error:#}");
}

#[test]
fn declining_stub_skips_and_does_nothing_else() {
    let wire = desc(0, "none");
    let engine =
        ComponentEngine::unavailable_declining("demo-component", &wire, "feedfacefeedface00", "absent").unwrap();
    let src = source("x");
    assert_eq!(engine.skip_reason(&src).as_deref(), Some("absent"));
    assert!(engine.lint(&src, &config(4)).unwrap().is_empty());
    assert!(matches!(
        engine.format(&src, &config(4)).unwrap(),
        FormatOutput::Unchanged
    ));
}
