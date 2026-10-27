//! One lint pass over a file's content: run (or reuse) every engine in the plan.

use std::sync::Arc;

use super::engine_clock::EngineClock;
use super::fast::decode_payload;
use super::fast::encode_payload;
use super::plan::EnginePlan;
use super::types::RunDebug;
use super::{guard_engine_panic, note_slow_engine, push_engine_debug};
use crate::discover::DiscoveredFile;
use crate::engine::{Diagnostic, SourceFile};

/// What one lint pass over a file produced.
pub(super) struct LintPass {
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) debug: Option<RunDebug>,
    /// Raw per-engine output for the record; empty unless asked to keep it.
    pub(super) payloads: Vec<Vec<u8>>,
}

/// Run every lint-capable engine for the file's language over `content`.
///
/// `reuse` holds, per engine in plan order, the output recorded for these exact
/// bytes under an unchanged engine key, which stands in for running the engine.
/// When `keep` is set the raw (pre-remap) output of every engine is returned for
/// the record. When `collect_debug` is set, also returns per-engine cache
/// hit/miss + timing; otherwise that element is `None`.
pub(super) fn lint_content(
    f: &DiscoveredFile,
    engine_plans: &[EnginePlan],
    content: &str,
    collect_debug: bool,
    reuse: &[Option<&[u8]>],
    keep: bool,
) -> anyhow::Result<LintPass> {
    let src = SourceFile {
        path: f.path.clone(),
        language: f.language.clone(),
        content: Arc::from(content),
    };
    let mut all = Vec::new();
    let mut payloads = Vec::new();
    let mut debug = collect_debug.then(RunDebug::default);
    for (index, plan) in engine_plans.iter().enumerate() {
        let reused = reuse.get(index).copied().flatten();
        if let Some(payload) = reused
            && let Some(mut diags) = decode_payload(payload)
        {
            push_engine_debug(debug.as_mut(), plan, None);
            if keep {
                payloads.push(payload.to_vec());
            }
            if !plan.severity_remap.is_empty() {
                plan.severity_remap.apply(&mut diags);
            }
            all.extend(diags);
            continue;
        }
        let clock = EngineClock::start();
        let mut diags = guard_engine_panic(plan.engine.name(), &f.path, || plan.engine.lint(&src, &plan.config))?;
        let time = clock.stop();
        note_slow_engine(&f.path, content.len(), plan.engine.name(), time);
        push_engine_debug(debug.as_mut(), plan, Some(clock.started()));
        // Record raw severities so a later config-only remap can be applied
        // identically on both reuse and recompute.
        if keep {
            payloads.push(encode_payload(&diags));
        }
        if !plan.severity_remap.is_empty() {
            plan.severity_remap.apply(&mut diags);
        }
        all.extend(diags);
    }
    Ok(LintPass {
        diagnostics: all,
        debug,
        payloads,
    })
}
