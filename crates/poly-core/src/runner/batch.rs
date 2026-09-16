//! Batch prefill: run the batch-capable formatters once over many files,
//! ahead of the per-file loop (ADR 0033).
//!
//! # Why this is a prefill rather than a restructuring
//!
//! The obvious implementation hoists the read-and-filter half of
//! [`super::format_one`] out of the per-file closure so files can be grouped
//! before any engine runs. That moves the run's most correctness-sensitive
//! decisions — is this binary, generated, hash-stamped, ignored, withdrawn —
//! into a second place that has to agree with the first forever.
//!
//! This module does not touch them. It runs *before* the loop, computes what
//! pass one of the format chain would produce for the files it can batch, and
//! hands the results over as a lookup table. `format_one` keeps every filter it
//! had, in the same order, and consults the table only once it has decided the
//! file is genuinely due for formatting.
//!
//! That makes both error directions harmless by construction:
//!
//! - **Over-inclusion** — the prefill formats a file `format_one` later skips.
//!   The entry is simply never read.
//! - **Under-inclusion** — no entry exists. `format_one` spawns the tool per
//!   file, exactly as it did before.
//!
//! So a bug here costs time, not correctness. The one thing it must never do is
//! hand back an answer belonging to different content, which is what
//! [`BatchPrefill::take`]'s input comparison rules out.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use poly_cache::{Namespace, ResultCache};
use rayon::prelude::*;
use rustc_hash::FxHashMap;

use super::plan::{EnginePlan, RunPlan};
use crate::discover::DiscoveredFile;
use crate::engine::{FormatOutput, SourceFile};
use crate::filter::is_binary;
use crate::language::Language;

/// Smallest group worth batching.
///
/// From the measured host figures — a `shfmt` spawn costs 17.3 ms, and
/// materializing plus reading one file back costs 0.54 ms — batching `n` files
/// costs `17.3 + 0.54n` against `17.3n` for the per-file path, so it already
/// wins at `n = 2`. Even the cheapest tool in the tier (`gofmt`, ~18 ms) has the
/// same shape. Two is therefore the honest floor rather than a round number.
const MIN_BATCH: usize = 2;

/// Ceiling on the bytes held live by a prefill.
///
/// The per-file loop's peak memory is bounded by the number of workers. A
/// prefill is not: it holds every member's content until the loop reaches it, so
/// on a large enough tree it would trade a bounded footprint for one that scales
/// with the corpus. Past this budget the remaining files simply take the
/// per-file path — slower, and exactly as correct.
///
/// Unchanged files cost their content once, not twice: the entry's input and
/// output are two handles on the same `Arc` (see `run_shard`), and around 80% of
/// files in a formatted repository are unchanged.
const MAX_PREFILL_BYTES: usize = 64 * 1024 * 1024;

/// One precomputed pass-one result.
struct PrefillEntry {
    /// The engine that produced `output`, checked on lookup so an entry can
    /// never answer for a different backend.
    engine: &'static str,
    /// The exact content the tool was fed. Compared before the output is used.
    input: Arc<str>,
    /// What the tool produced for it.
    output: Arc<str>,
}

/// Pass-one format results computed ahead of the per-file loop.
///
/// Keyed by path alone — a file only ever has one batched engine, since
/// [`sole_batchable_engine`] refuses a multi-engine chain — so a lookup borrows
/// the `&Path` the caller already holds instead of allocating a `PathBuf` on
/// every call. This is read once per engine per file inside the runner's
/// `par_iter`, which is not a place to allocate.
#[derive(Default)]
pub(super) struct BatchPrefill {
    entries: FxHashMap<PathBuf, PrefillEntry>,
}

impl BatchPrefill {
    /// The precomputed output for `path` under `engine`, but **only** if the
    /// batch was fed exactly `current`.
    ///
    /// The content comparison is the mis-attribution guard, and it is why a
    /// permuted or stale batch result cannot silently become some other file's
    /// formatting: an entry that does not match the content in hand is ignored
    /// and the per-file path runs instead. It also confines the prefill to pass
    /// one — by pass two `current` is the previous pass's output, which no batch
    /// was ever fed.
    pub(super) fn take(&self, path: &Path, engine: &str, current: &str) -> Option<Arc<str>> {
        let entry = self.entries.get(path)?;
        (entry.engine == engine && *entry.input == *current).then(|| Arc::clone(&entry.output))
    }

    /// How many pass-one results were precomputed. Reported under `--debug` so
    /// a run that silently stopped batching is visible rather than just slower.
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A file that can take part in a batch, with the content the batch will use.
struct Candidate {
    path: PathBuf,
    language: Language,
    content: Arc<str>,
    config_id: usize,
}

/// Compute pass-one results for every file whose format plan is a single
/// batch-capable engine.
///
/// Files already answered by the result cache are excluded: the per-file loop
/// will hit the cache for them and never spawn anything, so batching them would
/// be pure waste.
pub(super) fn prefill(files: &[DiscoveredFile], plan: &RunPlan, cache: &ResultCache) -> BatchPrefill {
    let mut candidates: Vec<Candidate> = files
        .par_iter()
        .filter_map(|file| candidate_for(file, plan, cache))
        .collect();
    if candidates.len() < MIN_BATCH {
        return BatchPrefill::default();
    }
    // Sorted so the budget below takes a deterministic prefix: which files get
    // batched must not depend on the order rayon happened to finish them in, or
    // two runs over one tree could disagree about what they did.
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(limit) = budget_prefix(&candidates) {
        tracing::debug!(
            batched = limit,
            total = candidates.len(),
            "batch prefill truncated at {MAX_PREFILL_BYTES} bytes; the rest take the per-file path"
        );
        candidates.truncate(limit);
    }

    // Group by the plan that governs the file. `(config_id, language)` is the
    // key the plan itself is stored under, so a group cannot span two
    // `EngineConfig`s — which is what keeps a batch's results consistent with
    // the cache key they will be filed under (ADR 0018).
    let mut groups: FxHashMap<(usize, Language), Vec<Candidate>> = FxHashMap::default();
    for candidate in candidates {
        groups
            .entry((candidate.config_id, candidate.language.clone()))
            .or_default()
            .push(candidate);
    }

    // Shard each group across the pool. One invocation is one process on one
    // core — `shfmt` has no internal parallelism to recover — so the shards, not
    // the tool, are where the parallelism comes from.
    let shards: Vec<(&EnginePlan, &[Candidate])> = groups
        .iter()
        .filter(|(_, group)| group.len() >= MIN_BATCH)
        .filter_map(|((config_id, language), group)| {
            let engine_plan = sole_batchable_engine(plan.engines(*config_id, language))?;
            Some((engine_plan, group))
        })
        .flat_map(|(engine_plan, group)| {
            shard_sizes(group.len())
                .into_iter()
                .scan(0usize, |offset, size| {
                    let start = *offset;
                    *offset += size;
                    Some(&group[start..*offset])
                })
                .map(move |shard| (engine_plan, shard))
                .collect::<Vec<_>>()
        })
        .collect();

    let entries: Vec<(PathBuf, PrefillEntry)> = shards.into_par_iter().flat_map(run_shard).collect();
    BatchPrefill {
        entries: entries.into_iter().collect(),
    }
}

/// Run one shard, returning its per-file entries.
///
/// Every failure mode returns nothing for the shard, which leaves those files to
/// the per-file path. That is the whole error story: a batch is an optimisation,
/// and an optimisation that cannot prove its answer declines to give one.
fn run_shard<'a>((engine_plan, shard): (&'a EnginePlan, &'a [Candidate])) -> Vec<(PathBuf, PrefillEntry)> {
    let batch: Vec<SourceFile> = shard
        .iter()
        .map(|candidate| SourceFile {
            path: candidate.path.clone(),
            language: candidate.language.clone(),
            content: Arc::clone(&candidate.content),
        })
        .collect();

    let outputs = match engine_plan.engine.format_batch(&batch, &engine_plan.config) {
        Ok(outputs) => outputs,
        Err(error) => {
            tracing::debug!(
                engine = engine_plan.engine.name(),
                files = batch.len(),
                "batch formatting declined; falling back to per-file: {error:#}"
            );
            return Vec::new();
        }
    };
    // A ragged answer is the one shape that would attribute one file's
    // formatting to another, so it is refused wholesale rather than zipped.
    if outputs.len() != batch.len() {
        tracing::warn!(
            engine = engine_plan.engine.name(),
            expected = batch.len(),
            got = outputs.len(),
            "batch returned the wrong number of results; discarding it"
        );
        return Vec::new();
    }

    let name = engine_plan.engine.name();
    batch
        .into_iter()
        .zip(outputs)
        .filter_map(|(src, output)| {
            let formatted = match output {
                Ok(FormatOutput::Unchanged) => Arc::clone(&src.content),
                Ok(FormatOutput::Formatted(text)) => Arc::from(text),
                // A per-file failure inside a successful batch: leave it to the
                // per-file path, which will produce (and attribute) the error.
                Err(_) => return None,
            };
            Some((
                src.path,
                PrefillEntry {
                    engine: name,
                    input: src.content,
                    output: formatted,
                },
            ))
        })
        .collect()
}

/// The engine plan for a file, when the file's whole format chain is a single
/// batch-capable engine.
///
/// Restricting to a one-engine chain is what makes a prefill sound without
/// modelling the chain: with one engine, pass one's input is the file's own
/// bytes. With two, the second engine's input is the first's output, which no
/// batch has seen. Every language this tier serves has a single format engine
/// today — the cross-cutting backends all declare `format: false` — but the
/// check is made per file rather than assumed, because a future format-capable
/// cross-cutting backend would otherwise silently corrupt the chain.
fn sole_batchable_engine(plans: &[EnginePlan]) -> Option<&EnginePlan> {
    match plans {
        [only] if only.engine.batch_support(&only.config).format => Some(only),
        _ => None,
    }
}

/// Decide whether one discovered file can join a batch, reading it if so.
fn candidate_for(file: &DiscoveredFile, plan: &RunPlan, cache: &ResultCache) -> Option<Candidate> {
    let engine_plan = sole_batchable_engine(plan.engines(file.config_id, &file.language))?;

    let bytes = std::fs::read(&file.path).ok()?;
    if is_binary(&bytes) {
        return None;
    }
    let content: Arc<str> = Arc::from(String::from_utf8(bytes).ok()?.as_str());

    // Already cached: the per-file loop answers it without spawning anything.
    if cache.enabled() {
        let digest = ResultCache::single_file_digest(&content);
        let key = ResultCache::key_with_args(
            Namespace::Fmt,
            engine_plan.engine.name(),
            engine_plan.engine.version(),
            &engine_plan.serialized_args,
            &digest,
        );
        if cache.get(Namespace::Fmt, &key).is_some() {
            return None;
        }
    }

    Some(Candidate {
        path: file.path.clone(),
        language: file.language.clone(),
        content,
        config_id: file.config_id,
    })
}

/// How many of `candidates` fit inside [`MAX_PREFILL_BYTES`], or `None` when
/// all of them do.
///
/// Always keeps at least one file, so a single file larger than the whole budget
/// is still batched rather than silently producing an empty prefill.
fn budget_prefix(candidates: &[Candidate]) -> Option<usize> {
    let mut total = 0usize;
    for (index, candidate) in candidates.iter().enumerate() {
        total = total.saturating_add(candidate.content.len());
        if total > MAX_PREFILL_BYTES {
            return Some(index.max(1));
        }
    }
    None
}

/// Split `total` files into at most one shard per worker, as evenly as possible
/// and never smaller than [`MIN_BATCH`].
fn shard_sizes(total: usize) -> Vec<usize> {
    let workers = rayon::current_num_threads().max(1);
    let shards = (total / MIN_BATCH).clamp(1, workers);
    let base = total / shards;
    let remainder = total % shards;
    (0..shards).map(|i| base + usize::from(i < remainder)).collect()
}

#[cfg(test)]
mod tests {
    use super::{MIN_BATCH, shard_sizes};

    /// Sharding must partition the group: a lost file is a file the run reports
    /// as checked without anything having formatted it.
    #[test]
    fn shard_sizes_partition_the_group() {
        for total in 0..500usize {
            let sizes = shard_sizes(total);
            assert_eq!(
                sizes.iter().sum::<usize>(),
                total,
                "sharding {total} lost or invented files"
            );
            assert!(!sizes.is_empty(), "sharding {total} produced no shards");
            if total >= MIN_BATCH {
                assert!(
                    sizes.iter().all(|&s| s > 0),
                    "sharding {total} produced an empty shard: {sizes:?}"
                );
            }
        }
    }

    /// The budget must bound memory without ever producing an empty prefill.
    #[test]
    fn the_prefill_budget_keeps_a_deterministic_nonempty_prefix() {
        use std::path::PathBuf;
        use std::sync::Arc;

        use super::{Candidate, MAX_PREFILL_BYTES, budget_prefix};
        use crate::language::Language;

        let make = |n: usize, size: usize| -> Vec<Candidate> {
            (0..n)
                .map(|i| Candidate {
                    path: PathBuf::from(format!("/repo/{i:05}.sh")),
                    language: Language::Shell,
                    content: Arc::from("x".repeat(size).as_str()),
                    config_id: 0,
                })
                .collect()
        };

        assert_eq!(
            budget_prefix(&make(10, 1024)),
            None,
            "a small corpus must not be truncated"
        );

        let oversized = make(4, MAX_PREFILL_BYTES);
        let limit = budget_prefix(&oversized).expect("an oversized corpus must be truncated");
        assert!(limit >= 1, "truncation must never empty the prefill");
        assert!(limit < oversized.len(), "truncation must actually drop something");

        // A single file bigger than the entire budget is still batched.
        let huge = make(1, MAX_PREFILL_BYTES + 1);
        assert_eq!(budget_prefix(&huge), Some(1));
    }

    /// Shards should be balanced — an uneven split leaves workers idle while one
    /// process does most of the work.
    #[test]
    fn shards_are_balanced_within_one_file() {
        let sizes = shard_sizes(1000);
        let min = sizes.iter().min().copied().unwrap_or(0);
        let max = sizes.iter().max().copied().unwrap_or(0);
        assert!(max - min <= 1, "shards differ by more than one file: {sizes:?}");
    }
}
