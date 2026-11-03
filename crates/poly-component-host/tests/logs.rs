//! Component response logs re-emitted through the host's `tracing` subscriber.
//!
//! Its own test binary: `tracing` caches callsite interest process-wide, so a parallel test that
//! logs with no subscriber installed can disable these callsites before the scoped one here sees
//! them.

mod common;

use std::sync::{Arc, Mutex};

use common::{FakeComponent, config, desc, describe_of, init, source};
use poly_component_host::{ComponentEngine, ComponentSlot};
use poly_engine_api::engine::Engine;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn response_logs_are_re_emitted_at_their_level() {
    let wire = desc(0, "none");
    let fake = FakeComponent::new(describe_of(wire.clone()));
    let slot = ComponentSlot::from_transport("demo-component", Some(describe_of(wire.clone())), init(), fake);
    let engine = ComponentEngine::new("demo-component", &wire, "0123456789abcdef0123456789abcdef", slot).unwrap();
    let capture = Capture::default();
    let sink = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        engine.lint(&source("x"), &config(4)).unwrap();
    });
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("WARN"), "{text}");
    assert!(text.contains("recovered from odd input"), "{text}");
    assert!(text.contains("demo::parser"), "{text}");
}
