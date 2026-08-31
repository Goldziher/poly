//! Driving poly over one root and turning the runs into records.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use poly_core::report::LintDocument;
use poly_core::{Config, LintRun, RunOptions};

use super::record::{CacheRecord, IdempotenceRecord, PhaseRecord, RuleRecord};

/// One rule's yield on one root: `(code, findings, files with a finding, top paths)`.
///
/// `top_paths` is what makes a single generated file dominating a rule visible
/// before anyone reads the aggregate and ships the rule on by default.
pub type RuleYield = (String, usize, usize, Vec<(String, usize)>);

/// Options every measurement shares.
///
/// `--no-workspace` is not expressible here because the whole-project phase is a
/// CLI concern; the in-process runner never runs it, which is exactly why the
/// harness drives `poly_core` directly. That matters for the local corpus: the
/// phase executes `cargo clippy` against the live worktree, so a harness that
/// went through the CLI would not be read-only.
fn options(no_cache: bool) -> RunOptions {
    RunOptions {
        no_cache,
        explicit_config: true,
        ..RunOptions::default()
    }
}

fn config() -> Config {
    Config::default()
}

/// Summarize a lint run into the phase record, including the skip breakdown that
/// makes a coverage regression visible before it becomes a failure.
pub fn lint_phase(root: &Path, ceiling_ms: u128) -> (LintRun, PhaseRecord) {
    let started = Instant::now();
    let run = poly_core::lint_run(&[root.to_path_buf()], &config(), &options(false), false, false)
        .expect("lint_run must not fail; a per-file failure is carried in `errors`");
    let elapsed_ms = started.elapsed().as_millis();

    let mut skips_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for skip in &run.skipped {
        *skips_by_reason.entry(skip.reason.clone()).or_default() += 1;
    }
    let record = PhaseRecord {
        elapsed_ms,
        ceiling_ms,
        files_discovered: run.checked + run.skipped.len() + run.errors.len(),
        checked: run.checked,
        skipped: run.skipped.len(),
        errored: run.errors.len(),
        skips_by_reason,
    };
    (run, record)
}

/// Format twice against a disposable copy and report whether the second pass
/// changed anything.
///
/// A second pass that still changes a file is two formatters disagreeing — the
/// convergence property `supersedes_generic_formatter` exists to protect — and
/// it means `poly fmt --fix` followed by `poly fmt --check` would not be clean.
pub fn idempotence(copy: &Path, ceiling_ms: u128) -> (PhaseRecord, IdempotenceRecord) {
    let started = Instant::now();
    let first = poly_core::format_run(&[copy.to_path_buf()], &config(), &options(true), true, false)
        .expect("format_run must not fail");
    let elapsed_ms = started.elapsed().as_millis();

    let second = poly_core::format_run(&[copy.to_path_buf()], &config(), &options(true), false, false)
        .expect("format_run must not fail");

    let non_idempotent: Vec<String> = second
        .results
        .iter()
        .filter(|result| result.changed)
        .map(|result| relative(copy, &result.path))
        .collect();

    let mut skips_by_reason: BTreeMap<String, usize> = BTreeMap::new();
    for skip in &first.skipped {
        *skips_by_reason.entry(skip.reason.clone()).or_default() += 1;
    }
    let phase = PhaseRecord {
        elapsed_ms,
        ceiling_ms,
        files_discovered: first.results.len() + first.errors.len(),
        checked: first.checked,
        skipped: first.skipped.len(),
        errored: first.errors.len(),
        skips_by_reason,
    };
    let idempotence = IdempotenceRecord {
        pass1_changed: first.results.iter().filter(|result| result.changed).count(),
        pass2_changed: non_idempotent.len(),
        non_idempotent,
        errored: second.errors.len(),
    };
    (phase, idempotence)
}

/// Compare a cold run, a warm run and a `--no-cache` run.
///
/// The warm comparison catches an unstable cache key — a miss on bytes that did
/// not change. The `--no-cache` comparison catches the dangerous direction: a
/// cache serving results computed under inputs that have since moved.
pub fn cache_behaviour(root: &Path, cold: &LintRun, cold_ms: u128) -> CacheRecord {
    let render = |run: &LintRun| serde_json::to_string(&LintDocument::from_run(run)).unwrap_or_default();
    let cold_output = render(cold);

    let started = Instant::now();
    let warm = poly_core::lint_run(&[root.to_path_buf()], &config(), &options(false), false, false)
        .expect("warm lint_run must not fail");
    let warm_ms = started.elapsed().as_millis();

    let uncached = poly_core::lint_run(&[root.to_path_buf()], &config(), &options(true), false, false)
        .expect("uncached lint_run must not fail");

    CacheRecord {
        cold_ms,
        warm_ms,
        warm_output_identical: render(&warm) == cold_output,
        nocache_output_identical: render(&uncached) == cold_output,
    }
}

/// Per-rule yield for one cross-cutting engine, which is what decides whether a
/// rule can ship on by default.
///
/// Runs the engine alone. `--only` narrows the plan to it, which makes the
/// attribution exact and the run far cheaper than a full lint — and it does not
/// charge the skip budget, so a single-engine measurement does not trip a
/// coverage gate that has nothing to do with what is being measured.
pub fn rule_yield(root: &Path, engine: &str) -> Vec<RuleYield> {
    let opts = RunOptions {
        only: vec![engine.to_string()],
        ..options(true)
    };
    let Ok(run) = poly_core::lint_run(&[root.to_path_buf()], &config(), &opts, false, false) else {
        return Vec::new();
    };

    let mut by_code: BTreeMap<String, (usize, usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for result in &run.results {
        let path = relative(root, &result.path);
        let mut seen_here: BTreeMap<String, ()> = BTreeMap::new();
        for diagnostic in &result.diagnostics {
            let Some(code) = diagnostic.code.as_deref() else {
                continue;
            };
            let entry = by_code.entry(code.to_string()).or_default();
            entry.0 += 1;
            *entry.2.entry(path.clone()).or_default() += 1;
            if seen_here.insert(code.to_string(), ()).is_none() {
                entry.1 += 1;
            }
        }
    }

    by_code
        .into_iter()
        .map(|(code, (findings, files, paths))| {
            let mut top: Vec<(String, usize)> = paths.into_iter().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            top.truncate(10);
            (code, findings, files, top)
        })
        .collect()
}

/// Turn a rule yield into records for one root.
pub fn rule_records(
    corpus: super::record::Corpus,
    root_name: &str,
    engine: &str,
    yields: Vec<RuleYield>,
) -> Vec<RuleRecord> {
    yields
        .into_iter()
        .map(|(code, findings, files_with_finding, top_paths)| RuleRecord {
            corpus,
            root: root_name.to_string(),
            engine: engine.to_string(),
            code,
            findings,
            files_with_finding,
            top_paths,
        })
        .collect()
}

/// A path relative to the root, so a record never carries someone's home
/// directory — and, for the local corpus, never names a private tree's layout
/// beyond what is inside it.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Where this run writes its cache.
///
/// Under the harness working directory, never inside the root: the local corpus
/// is the developer's own working trees, and the read-only guarantee is worth
/// nothing if the measurement leaves a cache directory behind in them. Keyed by
/// root name so two roots cannot share a cache and call the result a hit.
pub fn cache_home(root_name: &str) -> PathBuf {
    let base = std::env::var("POLY_HARDEN_ROOT").unwrap_or_else(|_| "/tmp/poly-harden".to_string());
    PathBuf::from(base).join("cache").join(root_name)
}
