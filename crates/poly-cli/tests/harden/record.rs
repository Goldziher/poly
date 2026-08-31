//! The NDJSON record one harden run leaves behind, one line per root.
//!
//! Every threshold a root is judged against is written into its own record —
//! ceilings, floors, the counts that were compared — so a result read months
//! later is self-describing rather than a number whose meaning has to be
//! reconstructed from the harness at that commit.
//!
//! **These types deliberately have no field that can hold source text.** The
//! harness runs over third-party repositories and uploads its results as a CI
//! artifact, so a snippet reaching a record would be redistribution. Findings
//! are carried as `path:line` and as counts; keep it that way.

use std::collections::BTreeMap;

use serde::Serialize;

/// Which corpus a root came from, and therefore what it is allowed to gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Corpus {
    /// Local sibling working trees. Unstable and never written to, so counts
    /// from here are trend data, never a gate.
    A,
    /// Third-party clones pinned to a commit. The only corpus whose counts can
    /// gate, because only a pinned input makes a count reproducible.
    B,
    /// Machine- or agent-generated code. Pinned, but the *population* is a
    /// judgement call, so counts are audit input rather than a gate.
    C,
}

// Which corpus may gate on counts is enforced where the decision belongs — the
// orchestrator picks the corpus, and `docs/harden-corpus.md` records the rule —
// rather than as a predicate here that nothing consults.

/// What one phase cost and what it covered.
#[derive(Debug, Default, Serialize)]
pub struct PhaseRecord {
    pub elapsed_ms: u128,
    pub ceiling_ms: u128,
    pub files_discovered: usize,
    pub checked: usize,
    pub skipped: usize,
    pub errored: usize,
    /// Skip reasons and how often each fired, so a coverage change is visible
    /// before it becomes a failure.
    pub skips_by_reason: BTreeMap<String, usize>,
}

/// The formatter-convergence result. A second pass that still changes a file
/// means two formatters disagree and the run never reaches a fixed point.
#[derive(Debug, Default, Serialize)]
pub struct IdempotenceRecord {
    pub pass1_changed: usize,
    pub pass2_changed: usize,
    /// Paths that changed on the second pass, relative to the root.
    pub non_idempotent: Vec<String>,
    pub errored: usize,
}

/// Cache behaviour across a cold run, a warm run and a `--no-cache` run.
#[derive(Debug, Default, Serialize)]
pub struct CacheRecord {
    pub cold_ms: u128,
    pub warm_ms: u128,
    /// Whether the warm run produced byte-identical output to the cold one.
    pub warm_output_identical: bool,
    /// Whether a `--no-cache` run agreed with the cached one. This is the
    /// dangerous direction: a cache serving results computed under different
    /// inputs.
    pub nocache_output_identical: bool,
}

/// One rule's yield on one root.
#[derive(Debug, Serialize)]
pub struct RuleRecord {
    pub corpus: Corpus,
    pub root: String,
    pub engine: String,
    pub code: String,
    pub findings: usize,
    pub files_with_finding: usize,
    /// The paths contributing the most findings, count only — what makes a
    /// single generated file dominating a rule's yield visible *before* someone
    /// reads the aggregate and ships the rule on.
    pub top_paths: Vec<(String, usize)>,
}

/// Everything one root produced.
#[derive(Debug, Serialize)]
pub struct RootRecord {
    pub schema: u32,
    pub corpus: Corpus,
    pub root: String,
    pub path: String,
    /// Commit the measurement describes, when the root is a git checkout.
    pub commit: Option<String>,
    /// Whether the tree had uncommitted changes when the run started. A dirty
    /// root's counts are not comparable with anything.
    pub dirty: bool,
    /// Whether it had them when the run finished. Different from `dirty` means
    /// the tree moved *during* the measurement, which invalidates every
    /// comparison the run made — the cache checks especially, since they compare
    /// two reads of the same tree.
    pub dirty_after: bool,
    pub poly_version: String,
    pub native_tools_enabled: bool,
    pub lint: PhaseRecord,
    pub fmt: PhaseRecord,
    pub idempotence: IdempotenceRecord,
    pub cache: CacheRecord,
    pub rules: Vec<RuleRecord>,
    /// Every assertion that failed, reported together rather than one per run.
    pub failures: Vec<String>,
}

impl RootRecord {
    /// Current schema version of [`RootRecord`].
    pub const SCHEMA: u32 = 1;
}
