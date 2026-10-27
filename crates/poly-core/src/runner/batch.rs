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
use crate::engine::{Diagnostic, FormatOutput, SourceFile};
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

/// The precomputed passes for one file: `(input, output)` per format pass.
///
/// Pass one's input is the file's own bytes; pass two's is pass one's output, and
/// so on until a pass changes nothing. Carrying the whole chain is what lets the
/// convergence loop confirm a file is stable without spawning the tool again:
/// of the files a formatter changes, the loop needs one more pass over every one
/// of them just to see that the second pass is a no-op.
struct PrefillEntry {
    /// The engine that produced the stages, checked on lookup so an entry can
    /// never answer for a different backend.
    engine: &'static str,
    /// Each stage's exact input, compared before its output is used.
    stages: Vec<(Arc<str>, Arc<str>)>,
}

/// Format results computed ahead of the per-file loop.
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
    /// and the per-file path runs instead.
    pub(super) fn take(&self, path: &Path, engine: &str, current: &str) -> Option<Arc<str>> {
        let entry = self.entries.get(path)?;
        if entry.engine != engine {
            return None;
        }
        entry
            .stages
            .iter()
            .find(|(input, _)| **input == *current)
            .map(|(_, output)| Arc::clone(output))
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

/// How many passes beyond the first a shard precomputes.
///
/// A formatter that changed a file is run once more over its output, which is
/// the pass that confirms it settled. Two covers a formatter that is idempotent
/// after one correction; anything that needs more is not converging in a way a
/// prefill should paper over, and the per-file loop takes it from there.
const CONFIRM_PASSES: usize = 2;

/// One file's progress through a shard's passes.
struct Member<'a> {
    candidate: &'a Candidate,
    /// What the next pass is fed: the previous pass's output.
    current: Arc<str>,
    stages: Vec<(Arc<str>, Arc<str>)>,
}

/// Run one shard, returning its per-file entries.
///
/// Every failure mode returns nothing for the files it touches, which leaves
/// those files to the per-file path. That is the whole error story: a batch is
/// an optimisation, and an optimisation that cannot prove its answer declines to
/// give one.
fn run_shard<'a>((engine_plan, shard): (&'a EnginePlan, &'a [Candidate])) -> Vec<(PathBuf, PrefillEntry)> {
    let mut members: Vec<Member<'_>> = shard
        .iter()
        .map(|candidate| Member {
            candidate,
            current: Arc::clone(&candidate.content),
            stages: Vec::new(),
        })
        .collect();

    let mut active: Vec<usize> = (0..members.len()).collect();
    for _ in 0..=CONFIRM_PASSES {
        if active.is_empty() {
            break;
        }
        let batch: Vec<SourceFile> = active
            .iter()
            .map(|&index| SourceFile {
                path: members[index].candidate.path.clone(),
                language: members[index].candidate.language.clone(),
                content: Arc::clone(&members[index].current),
            })
            .collect();
        let outputs = match engine_plan.engine.format_batch(&batch, &engine_plan.config) {
            Ok(outputs) if outputs.len() == batch.len() => outputs,
            // A ragged answer is the one shape that would attribute one file's
            // formatting to another, so it is refused wholesale rather than zipped.
            Ok(outputs) => {
                tracing::warn!(
                    engine = engine_plan.engine.name(),
                    expected = batch.len(),
                    got = outputs.len(),
                    "batch returned the wrong number of results; discarding it"
                );
                break;
            }
            Err(error) => {
                tracing::debug!(
                    engine = engine_plan.engine.name(),
                    files = batch.len(),
                    "batch formatting declined; falling back to per-file: {error:#}"
                );
                break;
            }
        };
        let mut changed = Vec::new();
        for (&index, output) in active.iter().zip(outputs) {
            let member = &mut members[index];
            // A per-file failure inside a successful batch: leave it to the
            // per-file path, which will produce (and attribute) the error.
            let Ok(output) = output else { continue };
            let next = match output {
                FormatOutput::Unchanged => Arc::clone(&member.current),
                FormatOutput::Formatted(text) => Arc::from(text),
            };
            let moved = *next != *member.current;
            member
                .stages
                .push((std::mem::replace(&mut member.current, Arc::clone(&next)), next));
            if moved {
                changed.push(index);
            }
        }
        active = changed;
    }

    let name = engine_plan.engine.name();
    members
        .into_iter()
        .filter(|member| !member.stages.is_empty())
        .map(|member| {
            (
                member.candidate.path.clone(),
                PrefillEntry {
                    engine: name,
                    stages: member.stages,
                },
            )
        })
        .collect()
}

/// One precomputed lint result.
struct LintEntry {
    engine: &'static str,
    /// The exact content the tool was fed. Compared before the result is used.
    input: Arc<str>,
    diagnostics: Vec<Diagnostic>,
}

/// Lint results computed ahead of the per-file loop for the backends that batch
/// (`shellcheck`).
///
/// Unlike the format chain, lint engines are independent — each reads the file's
/// own bytes and none consumes another's output — so any number of a language's
/// engines may batch and there is no single-engine restriction. A path can
/// therefore carry several entries, one per engine.
#[derive(Default)]
pub(super) struct LintPrefill {
    entries: FxHashMap<PathBuf, Vec<LintEntry>>,
}

impl LintPrefill {
    /// The precomputed diagnostics for `path` under `engine`, but only if the
    /// batch was fed exactly `content`. See [`BatchPrefill::take`]: the content
    /// comparison is what makes a stale or permuted entry impossible to apply,
    /// and what confines the prefill to the content the run started with (a
    /// `--fix` pass lints different bytes and so runs the tool itself).
    pub(super) fn take(&self, path: &Path, engine: &str, content: &str) -> Option<Vec<Diagnostic>> {
        self.entries
            .get(path)?
            .iter()
            .find(|entry| entry.engine == engine && *entry.input == *content)
            .map(|entry| entry.diagnostics.clone())
    }

    /// How many results were precomputed, for the same reason as
    /// [`BatchPrefill::len`].
    pub(super) fn len(&self) -> usize {
        self.entries.values().map(Vec::len).sum()
    }
}

/// Compute lint results for every file a batching lint engine serves.
///
/// Files already answered by the result cache are excluded, as for formatting:
/// the per-file loop answers them without spawning anything. Every failure mode
/// returns nothing for the shard, which leaves its files to the per-file path.
pub(super) fn prefill_lint(files: &[DiscoveredFile], plan: &RunPlan, cache: &ResultCache) -> LintPrefill {
    let mut candidates: Vec<(usize, Candidate)> = files
        .par_iter()
        .flat_map_iter(|file| lint_candidates_for(file, plan, cache))
        .collect();
    if candidates.len() < MIN_BATCH {
        return LintPrefill::default();
    }
    candidates.sort_by(|a, b| (a.0, &a.1.path).cmp(&(b.0, &b.1.path)));

    let mut groups: FxHashMap<(usize, Language, usize), Vec<Candidate>> = FxHashMap::default();
    for (engine_index, candidate) in candidates {
        groups
            .entry((candidate.config_id, candidate.language.clone(), engine_index))
            .or_default()
            .push(candidate);
    }
    let shards: Vec<(&EnginePlan, &[Candidate])> = groups
        .iter()
        .filter(|(_, group)| group.len() >= MIN_BATCH)
        .flat_map(|((config_id, language, engine_index), group)| {
            let engine_plan = &plan.engines(*config_id, language)[*engine_index];
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

    let mut entries: FxHashMap<PathBuf, Vec<LintEntry>> = FxHashMap::default();
    for (path, entry) in shards.into_par_iter().flat_map(run_lint_shard).collect::<Vec<_>>() {
        entries.entry(path).or_default().push(entry);
    }
    LintPrefill { entries }
}

/// The `(engine index, candidate)` pairs one file contributes: one per planned
/// engine that batches, none when the file cannot be read as text or the cache
/// already holds that engine's answer.
fn lint_candidates_for(file: &DiscoveredFile, plan: &RunPlan, cache: &ResultCache) -> Vec<(usize, Candidate)> {
    let engines = plan.engines(file.config_id, &file.language);
    let batching: Vec<usize> = engines
        .iter()
        .enumerate()
        .filter(|(_, engine_plan)| {
            engine_plan.provides_language_lint && engine_plan.engine.batch_support(&engine_plan.config).lint
        })
        .map(|(index, _)| index)
        .collect();
    if batching.is_empty() {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(&file.path) else {
        return Vec::new();
    };
    if is_binary(&bytes) {
        return Vec::new();
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Vec::new();
    };
    let content: Arc<str> = Arc::from(text.as_str());
    let digest = cache
        .enabled()
        .then(|| ResultCache::single_file_digest_with_path(&file.path.to_string_lossy(), &content));

    batching
        .into_iter()
        .filter(|&index| {
            let engine_plan = &engines[index];
            digest.as_ref().is_none_or(|digest| {
                let key = ResultCache::key_with_args(
                    Namespace::Lint,
                    engine_plan.engine.name(),
                    engine_plan.engine.version(),
                    &engine_plan.serialized_args,
                    digest,
                );
                cache.get(Namespace::Lint, &key).is_none()
            })
        })
        .map(|index| {
            (
                index,
                Candidate {
                    path: file.path.clone(),
                    language: file.language.clone(),
                    content: Arc::clone(&content),
                    config_id: file.config_id,
                },
            )
        })
        .collect()
}

/// Run one lint shard, returning its per-file entries. As for formatting, every
/// failure returns nothing and leaves the files to the per-file path.
fn run_lint_shard((engine_plan, shard): (&EnginePlan, &[Candidate])) -> Vec<(PathBuf, LintEntry)> {
    let batch: Vec<SourceFile> = shard
        .iter()
        .map(|candidate| SourceFile {
            path: candidate.path.clone(),
            language: candidate.language.clone(),
            content: Arc::clone(&candidate.content),
        })
        .collect();
    let outputs = match engine_plan.engine.lint_batch(&batch, &engine_plan.config) {
        Ok(outputs) if outputs.len() == batch.len() => outputs,
        Ok(outputs) => {
            tracing::warn!(
                engine = engine_plan.engine.name(),
                expected = batch.len(),
                got = outputs.len(),
                "batch lint returned the wrong number of results; discarding it"
            );
            return Vec::new();
        }
        Err(error) => {
            tracing::debug!(
                engine = engine_plan.engine.name(),
                files = batch.len(),
                "batch linting declined; falling back to per-file: {error:#}"
            );
            return Vec::new();
        }
    };
    let name = engine_plan.engine.name();
    batch
        .into_iter()
        .zip(outputs)
        .filter_map(|(src, output)| {
            let diagnostics = output.ok()?;
            Some((
                src.path,
                LintEntry {
                    engine: name,
                    input: src.content,
                    diagnostics,
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

    mod confirmation_passes {
        //! A formatter that changed a file must be run once more over its output
        //! to prove it settled, and that second run is as much a spawn as the
        //! first. The batch carries it, so the loop never has to.

        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use poly_cache::ResultCache;

        use super::super::{Candidate, PrefillEntry, prefill, run_shard};
        use crate::config::{EngineConfig, GlobalDefaults};
        use crate::discover::DiscoveredFile;
        use crate::engine::{BatchSupport, Capabilities, Engine, FormatOutput, SourceFile};
        use crate::filter::SeverityRemap;
        use crate::language::Language;
        use crate::runner::plan::{EnginePlan, RunPlan};

        /// Appends `x` until a line holds three, then leaves it alone — settled
        /// after two changing passes, so the loop needs three to see it.
        struct Appender {
            batches: Arc<AtomicUsize>,
        }

        impl Engine for Appender {
            fn name(&self) -> &'static str {
                "appender"
            }

            fn languages(&self) -> &'static [Language] {
                &[Language::Shell]
            }

            fn capabilities(&self) -> Capabilities {
                Capabilities {
                    lint: false,
                    format: true,
                    fix: false,
                }
            }

            fn version(&self) -> &str {
                "test"
            }

            fn batch_support(&self, _cfg: &EngineConfig) -> BatchSupport {
                BatchSupport {
                    format: true,
                    lint: false,
                }
            }

            fn format_batch(
                &self,
                batch: &[SourceFile],
                _cfg: &EngineConfig,
            ) -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
                self.batches.fetch_add(1, Ordering::SeqCst);
                Ok(batch
                    .iter()
                    .map(|src| {
                        if src.content.trim_end().len() >= 3 {
                            Ok(FormatOutput::Unchanged)
                        } else {
                            Ok(FormatOutput::Formatted(format!("{}x\n", src.content.trim_end())))
                        }
                    })
                    .collect())
            }
        }

        fn plan(batches: &Arc<AtomicUsize>) -> RunPlan {
            RunPlan::serving(
                0,
                Language::Shell,
                vec![EnginePlan {
                    engine: Box::new(Appender {
                        batches: Arc::clone(batches),
                    }),
                    config: EngineConfig {
                        globals: GlobalDefaults::default(),
                        indent_width: 2,
                        options: toml::Table::new(),
                    },
                    serialized_args: ResultCache::serialize_args(&toml::Table::new()),
                    severity_remap: SeverityRemap::new(Vec::new()),
                    provides_language_lint: false,
                }],
            )
        }

        #[test]
        fn the_confirming_pass_is_precomputed_so_the_loop_spawns_nothing() {
            let directory = tempfile::tempdir().expect("tempdir");
            let cache = ResultCache::open(directory.path().join("cache"), false).expect("open disabled cache");
            let files: Vec<DiscoveredFile> = ["a", "b"]
                .iter()
                .map(|name| {
                    let path = directory.path().join(format!("{name}.sh"));
                    std::fs::write(&path, "a\n").expect("write fixture");
                    DiscoveredFile {
                        path,
                        language: Language::Shell,
                        config_id: 0,
                    }
                })
                .collect();
            let batches = Arc::new(AtomicUsize::new(0));
            let prefilled = prefill(&files, &plan(&batches), &cache);

            let path = &files[0].path;
            let first = prefilled.take(path, "appender", "a\n").expect("pass one");
            assert_eq!(&*first, "ax\n");
            let second = prefilled.take(path, "appender", &first).expect("pass two");
            assert_eq!(&*second, "axx\n");
            let third = prefilled.take(path, "appender", &second).expect("the confirming pass");
            assert_eq!(&*third, "axx\n", "the last stage proves it settled");
            assert!(
                prefilled.take(path, "appender", "something else\n").is_none(),
                "content the batch never saw must not be answered"
            );
            assert!(prefilled.take(path, "another-engine", "a\n").is_none());
            assert_eq!(
                batches.load(Ordering::SeqCst),
                3,
                "one spawn per pass for the whole shard, not one per file per pass"
            );
        }

        #[test]
        fn each_member_stops_at_its_own_fixed_point() {
            let batches = Arc::new(AtomicUsize::new(0));
            let candidates: Vec<Candidate> = ["a\n", "bb\n"]
                .iter()
                .enumerate()
                .map(|(index, content)| Candidate {
                    path: format!("/repo/{index}.sh").into(),
                    language: Language::Shell,
                    content: Arc::from(*content),
                    config_id: 0,
                })
                .collect();
            let plan = plan(&batches);
            let engine_plan = &plan.engines(0, &Language::Shell)[0];

            let entries: Vec<(std::path::PathBuf, PrefillEntry)> = run_shard((engine_plan, &candidates));

            assert_eq!(entries.len(), 2);
            let longer = entries.iter().find(|(path, _)| path.ends_with("1.sh")).expect("entry");
            assert_eq!(longer.1.stages.len(), 2, "bb -> bbx, then bbx confirmed");
            let shorter = entries.iter().find(|(path, _)| path.ends_with("0.sh")).expect("entry");
            assert_eq!(shorter.1.stages.len(), 3, "a -> ax -> axx, then axx confirmed");
        }
    }
}
