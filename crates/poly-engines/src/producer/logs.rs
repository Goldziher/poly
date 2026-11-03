//! Capturing the `tracing` events an engine emits during one call into [`WireLog`] records.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use poly_component_protocol::{WireLog, WireLogLevel};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// Records collected during one call.
#[derive(Default)]
pub(crate) struct LogCapture {
    records: Arc<Mutex<Vec<WireLog>>>,
}

struct CaptureLayer {
    records: Arc<Mutex<Vec<WireLog>>>,
    max: WireLogLevel,
}

impl LogCapture {
    /// Run `operation` on this thread with a subscriber that keeps events up to `max` verbosity.
    pub(crate) fn run<T>(&self, max: WireLogLevel, operation: impl FnOnce() -> T) -> T {
        let layer = CaptureLayer {
            records: Arc::clone(&self.records),
            max,
        };
        let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
        tracing::dispatcher::with_default(&dispatch, operation)
    }

    /// The records captured so far, leaving the capture empty.
    pub(crate) fn take(&self) -> Vec<WireLog> {
        std::mem::take(&mut *self.records.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

fn rank(level: WireLogLevel) -> u8 {
    match level {
        WireLogLevel::Error => 0,
        WireLogLevel::Warn => 1,
        WireLogLevel::Info => 2,
        WireLogLevel::Debug => 3,
        WireLogLevel::Trace => 4,
    }
}

fn wire_level(level: &Level) -> WireLogLevel {
    if *level == Level::ERROR {
        WireLogLevel::Error
    } else if *level == Level::WARN {
        WireLogLevel::Warn
    } else if *level == Level::INFO {
        WireLogLevel::Info
    } else if *level == Level::DEBUG {
        WireLogLevel::Debug
    } else {
        WireLogLevel::Trace
    }
}

/// Collects the `message` field first and every other field as ` name=value`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = wire_level(metadata.level());
        if rank(level) > rank(self.max) {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        visitor.message.push_str(&visitor.fields);
        self.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(WireLog {
                level,
                target: metadata.target().to_owned(),
                message: visitor.message,
            });
    }
}
