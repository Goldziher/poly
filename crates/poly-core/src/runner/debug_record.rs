//! The per-engine `--debug` record: where each engine's answer for a file came
//! from (ran, the result cache, or a batched invocation) and how long it took.

use super::plan::EnginePlan;
use super::types::{EngineDebug, RunDebug};

/// Where one engine's answer for a file came from, for its [`EngineDebug`] record.
#[derive(Clone, Copy)]
pub(super) enum DebugSource {
    /// The engine ran on this file, starting at this instant.
    Ran(std::time::Instant),
    /// The content-hash cache answered; the engine did not run.
    CacheHit,
    /// A batched invocation in this run answered for this file among others.
    Batched,
}

/// Append one [`EngineDebug`] record when debug collection is active.
pub(super) fn push_engine_debug(debug: Option<&mut RunDebug>, plan: &EnginePlan, source: DebugSource) {
    if let Some(debug) = debug {
        let duration_ms = match source {
            DebugSource::Ran(start) => start.elapsed().as_secs_f64() * 1000.0,
            DebugSource::CacheHit | DebugSource::Batched => 0.0,
        };
        debug.engines.push(EngineDebug {
            engine: plan.engine.name().to_owned(),
            version: plan.engine.version().to_owned(),
            duration_ms,
            cache_hit: matches!(source, DebugSource::CacheHit),
            batched: matches!(source, DebugSource::Batched),
        });
    }
}
