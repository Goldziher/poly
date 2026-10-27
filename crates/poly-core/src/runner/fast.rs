//! The stat-validated fast path and the per-file record that backs it.
//!
//! [`FastPath::resolve`] runs before a file is opened. It answers from the
//! [`StatIndex`] when the file's `stat`, the run's scope and every engine's key
//! equal what was recorded, and falls back to a content digest when only the
//! `stat` moved (a `touch`, a checkout that rewrote identical bytes). Anything
//! else is a [`Resolution::Miss`] carrying a [`Probe`]: the slow path reads the
//! file, reuses whichever recorded engine segments are still valid for its
//! bytes, and records the outcome against the `stat` taken **before** it read.
//!
//! What goes into the keys is the whole correctness argument. A **scope** key
//! folds in the cache format, the build identity, the run switches that change
//! a verdict without changing an engine (`[discovery] generated`,
//! `--fix-generated`) and the withdrawal reason if an instruction emptied the
//! plan. An **engine** key folds in the engine's name, `version()` and
//! serialized args — which already carry the config's options and the engine's
//! [`run_context`] — and, per file, its [`file_context`].
//!
//! [`run_context`]: crate::engine::Engine::run_context
//! [`file_context`]: crate::engine::Engine::file_context

use poly_cache::{
    ContentDigest, FileStat, Namespace, PathKey, PlanKey, Record, ResultCache, Segment, StatIndex, Verdict,
};
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::discover::DiscoveredFile;
use crate::engine::Diagnostic;
use crate::language::Language;

use super::plan::{EnginePlan, RunPlan, provides_language_lint};
use super::skips::SkippedFile;
use super::types::LintResult;
use crate::filter::suppressed::SuppressedDiagnostic;

/// How the run's files were resolved, reported once at the end so a test (or a
/// reader with `RUST_LOG=poly_core=debug`) can see whether the fast path worked.
#[derive(Default)]
struct Tally {
    stat_hits: AtomicUsize,
    content_hits: AtomicUsize,
    misses: AtomicUsize,
}

/// What let a recorded verdict stand in for a lint pass.
enum VouchedBy {
    Stat,
    Content,
}

pub(super) struct FastPath<'a> {
    index: &'a StatIndex,
    tally: Tally,
    plan: &'a RunPlan,
    scopes: FxHashMap<(usize, Language), PlanKey>,
    engine_keys: FxHashMap<(usize, Language), Vec<PlanKey>>,
    replay: Replay,
}

/// What a stat hit may replay without opening the file.
#[derive(Clone, Copy)]
pub(super) struct Replay {
    /// Reported findings: not while fixing (the content is needed to apply
    /// them) and not while collecting per-engine debug.
    pub(super) diagnostics: bool,
    /// Any verdict at all: not while collecting debug, which reports per-engine
    /// timing a replay never produced.
    pub(super) verdicts: bool,
}

/// A file the index could not vouch for, with what is needed to record it.
pub(super) struct Probe<'a> {
    path: PathKey,
    stat: FileStat,
    scope: PlanKey,
    keys: Vec<PlanKey>,
    prior: Option<&'a Record>,
}

pub(super) enum Resolution<'a> {
    Hit(&'a Record),
    Miss(Probe<'a>),
    /// The file could not be stat-ed; the slow path will report why.
    Unavailable,
}

impl<'a> FastPath<'a> {
    /// A fast path for `namespace`, or `None` when the cache is disabled.
    ///
    /// `flags(config_id)` supplies the per-config run switches that change a
    /// verdict without changing any engine.
    pub(super) fn new(
        cache: &'a ResultCache,
        namespace: Namespace,
        plan: &'a RunPlan,
        flags: &dyn Fn(usize) -> u8,
        replay: Replay,
    ) -> Option<Self> {
        if !cache.enabled() {
            return None;
        }
        let mut scopes = FxHashMap::default();
        let mut engine_keys = FxHashMap::default();
        for ((config_id, language), engines) in plan.iter() {
            let mut scope = PlanKey::builder(namespace);
            scope.part([flags(*config_id)]);
            scope.part(
                plan.withdrawal(*config_id, language)
                    .map(|withdrawal| withdrawal.reason())
                    .unwrap_or_default(),
            );
            let key = (*config_id, language.clone());
            scopes.insert(key.clone(), scope.finish());
            engine_keys.insert(key, engines.iter().map(|engine| base_key(namespace, engine)).collect());
        }
        Some(Self {
            index: cache.stat_index(namespace),
            tally: Tally::default(),
            plan,
            scopes,
            engine_keys,
            replay,
        })
    }

    /// The stat and keys for `file`, without consulting the index.
    pub(super) fn probe(&self, file: &DiscoveredFile) -> Option<Probe<'a>> {
        let stat = FileStat::of(&file.path).ok()?;
        let pair = (file.config_id, file.language.clone());
        let scope = *self.scopes.get(&pair)?;
        let engines = self.plan.engines(file.config_id, &file.language);
        let keys = self
            .engine_keys
            .get(&pair)?
            .iter()
            .zip(engines)
            .map(
                |(base, engine)| match engine.engine.file_context(&file.path, &engine.config) {
                    Some(context) => base.extended([context.as_str()]),
                    None => *base,
                },
            )
            .collect();
        let path = PathKey::of(&file.path);
        let prior = self.index.get(&path).filter(|record| record.scope == scope);
        Some(Probe {
            path,
            stat,
            scope,
            keys,
            prior,
        })
    }

    fn may_replay(&self, record: &Record) -> bool {
        self.replay.verdicts
            && match record.verdict {
                Verdict::Binary => true,
                Verdict::Clean => record.replayable,
                Verdict::Diagnostics => record.replayable && self.replay.diagnostics,
            }
    }

    pub(super) fn resolve(&self, file: &DiscoveredFile) -> Resolution<'a> {
        let Some(probe) = self.probe(file) else {
            return Resolution::Unavailable;
        };
        match self.vouched_for(file, &probe) {
            Some((record, how)) => {
                let counter = match how {
                    VouchedBy::Stat => &self.tally.stat_hits,
                    VouchedBy::Content => &self.tally.content_hits,
                };
                counter.fetch_add(1, Ordering::Relaxed);
                Resolution::Hit(record)
            }
            None => {
                self.tally.misses.fetch_add(1, Ordering::Relaxed);
                Resolution::Miss(probe)
            }
        }
    }

    /// The record that lets `file` be answered without a lint pass, and which
    /// tally line to count it on.
    fn vouched_for(&self, file: &DiscoveredFile, probe: &Probe<'a>) -> Option<(&'a Record, VouchedBy)> {
        let record = probe.prior?;
        if !record.has_keys(&probe.keys) || !self.may_replay(record) {
            return None;
        }
        if record.trusts(&probe.stat) {
            return Some((record, VouchedBy::Stat));
        }
        let bytes = std::fs::read(&file.path).ok()?;
        if record.digest != ContentDigest::of(&bytes) {
            return None;
        }
        self.index.put(
            probe.path,
            Record {
                stat: probe.stat,
                ..record.clone()
            },
        );
        Some((record, VouchedBy::Content))
    }

    /// Report how the run's files were resolved.
    pub(super) fn log_summary(&self, run: &str) {
        tracing::debug!(
            run,
            stat_hits = self.tally.stat_hits.load(Ordering::Relaxed),
            content_hits = self.tally.content_hits.load(Ordering::Relaxed),
            misses = self.tally.misses.load(Ordering::Relaxed),
            "stat index resolution"
        );
    }

    /// The diagnostics a recorded file produced, severity overrides applied, or
    /// `None` if a payload no longer decodes.
    pub(super) fn decode(&self, file: &DiscoveredFile, record: &Record) -> Option<Vec<Diagnostic>> {
        let engines = self.plan.engines(file.config_id, &file.language);
        let mut all = Vec::new();
        for (engine, segment) in engines.iter().zip(&record.segments) {
            let mut diagnostics = decode_payload(&segment.payload)?;
            if !engine.severity_remap.is_empty() {
                engine.severity_remap.apply(&mut diagnostics);
            }
            all.extend(diagnostics);
        }
        Some(all)
    }

    /// Remember how `probe`'s file, whose bytes are `bytes`, was judged.
    pub(super) fn record(&self, probe: &Probe, bytes: &[u8], outcome: Outcome) {
        let segments = probe
            .keys
            .iter()
            .zip(outcome.payloads.into_iter().chain(std::iter::repeat_with(Vec::new)))
            .map(|(key, payload)| Segment { key: *key, payload })
            .collect();
        self.index.put(
            probe.path,
            Record {
                stat: probe.stat,
                scope: probe.scope,
                digest: ContentDigest::of(bytes),
                verdict: outcome.verdict,
                replayable: outcome.replayable,
                segments,
            },
        );
    }
}

/// What a slow-path run concluded about one file.
pub(super) struct Outcome {
    pub(super) verdict: Verdict,
    pub(super) replayable: bool,
    /// Raw per-engine output in plan order; empty for fmt, where only the
    /// verdict is remembered.
    pub(super) payloads: Vec<Vec<u8>>,
}

impl Outcome {
    pub(super) fn binary() -> Self {
        Self {
            verdict: Verdict::Binary,
            replayable: true,
            payloads: Vec::new(),
        }
    }
}

impl Probe<'_> {
    /// The recorded output of each engine that is still valid for `bytes`:
    /// the same bytes, produced under the same engine key.
    pub(super) fn reusable(&self, bytes: &[u8]) -> Vec<Option<&[u8]>> {
        let Some(prior) = self.prior else {
            return Vec::new();
        };
        if prior.digest != ContentDigest::of(bytes) {
            return Vec::new();
        }
        self.keys
            .iter()
            .zip(&prior.segments)
            .map(|(key, segment)| (segment.key == *key).then_some(segment.payload.as_slice()))
            .collect()
    }
}

fn base_key(namespace: Namespace, engine: &EnginePlan) -> PlanKey {
    let mut builder = PlanKey::builder(namespace);
    builder
        .part(engine.engine.name())
        .part(engine.engine.version())
        .part(engine.serialized_args.as_str());
    builder.finish()
}

/// Decode one engine's recorded output; an empty payload is "nothing found".
pub(super) fn decode_payload(payload: &[u8]) -> Option<Vec<Diagnostic>> {
    if payload.is_empty() {
        return Some(Vec::new());
    }
    serde_json::from_slice(payload).ok()
}

/// Encode one engine's raw output for a record; "nothing found" is empty.
pub(super) fn encode_payload(diagnostics: &[Diagnostic]) -> Vec<u8> {
    if diagnostics.is_empty() {
        Vec::new()
    } else {
        serde_json::to_vec(diagnostics).unwrap_or_default()
    }
}

/// Persist the verdicts recorded this run. Failing to is never the run's failure:
/// the next run simply finds fewer records.
pub(super) fn flush_stat_indexes(cache: &ResultCache) {
    if let Err(error) = cache.flush_stat_indexes() {
        tracing::warn!("failed to store the stat index: {error:#}");
    }
}

/// Why a file that was read and found clean still does not count as linted, if
/// it does not.
pub(super) fn lint_skip_reason(plan: &RunPlan, f: &DiscoveredFile, externally_linted: &[Language]) -> Option<String> {
    let engine_plans = plan.engines(f.config_id, &f.language);
    // A language nothing holds rules for is not a clean file: the cross-cutting
    // backends still run and can still report findings, but no rule in this run
    // knows the language, so the file must not be counted as linted.
    //
    // `externally_linted` is the caller's declaration that another phase of the
    // *same* run does lint the language (`cargo clippy` over Rust). A linear
    // scan is the right shape here: the list holds one entry per whole-project
    // tool, so it is a handful of comparisons against a slice already in cache —
    // cheaper than the hash it would take to avoid them.
    //
    // A caller who narrowed the run with `--only` gets their own reason
    // instead: nothing linted the file, but "no lint rules for Rust" would
    // blame poly for a restriction the invocation asked for.
    (!provides_language_lint(engine_plans) && !externally_linted.contains(&f.language)).then(|| {
        match plan.withdrawal(f.config_id, &f.language) {
            Some(withdrawal) => withdrawal.reason(),
            None => SkippedFile::no_lint_rules_reason(&f.language),
        }
    })
}

/// The result for a file whose findings were replayed from its record.
pub(super) fn replayed_lint_result(
    f: &DiscoveredFile,
    diagnostics: Vec<Diagnostic>,
    suppressed: Vec<SuppressedDiagnostic>,
    skipped: Option<String>,
) -> LintResult {
    LintResult {
        path: f.path.clone(),
        config: f.config_id,
        diagnostics,
        suppressed,
        fix_withheld_generated: false,
        fixed: 0,
        skipped,
        error: None,
        debug: None,
    }
}

pub(super) fn log_fast_path(fast: Option<&FastPath>, run: &str) {
    if let Some(fast) = fast {
        fast.log_summary(run);
    }
}

#[cfg(test)]
mod tests;
