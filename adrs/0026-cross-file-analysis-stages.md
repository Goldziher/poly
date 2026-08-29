# 0026 — Cross-File Analysis Stages

- Status: Accepted
- Date: 2026-08-29

## Context

`Engine::lint(&self, src, cfg)` is per-file and pure (ADR 0004), and the blake3 result cache
is keyed per file — `(namespace, engine name, engine version, resolved config, file bytes)`
(ADR 0008). The rayon runner assumes the same thing: files are independent work items with no
shared mutable state (ADR 0009). Duplicate-code (DRY) detection is the first thing poly has
wanted to do that none of those assumptions can express — a clone is a relationship *between*
files, not a property *of* one — so this ADR states the exception explicitly rather than
letting it become an undocumented deviation from ADR 0008 and ADR 0009.

`poly-workspace` is not the home for it either: that crate orchestrates *external subprocess*
tools (`cargo clippy`, `cargo-sort`, …) against the live worktree for the whole-project lint
phase, and provides no cross-file AST index of its own to build a clone detector on top of.

## Decision

Duplicate-code detection is a **two-phase stage in the runner**, not an `Engine`.

- **Phase 1 (map): per-file fingerprints.** Winnowing (MOSS) over a rolling hash of a
  normalized token stream built from tree-sitter leaf nodes. This is folded into the
  **existing** `files.par_iter()` closure, so each file is read and decoded exactly once —
  it is not a second walk. The fingerprinting function is a pure function of `(file bytes, dry
  config)`, so it fits the existing cache discipline and is cached under a new
  `Namespace::Dry`.
- **Phase 2 (reduce): a sequential in-memory fold.** All per-file artifacts are folded into
  clone groups, which are merged into `Vec<LintResult>` **before** the filter that drops
  findings-free results — merging after it would silently drop a file whose only finding is a
  DRY hit.
- **A cache hit must still contribute its fingerprints to the reduce.** This is the subtle
  part, and it is where DRY breaks the lint cache's usual contract: a lint cache hit
  short-circuits the file entirely, but a DRY cache hit cannot, because the reduce needs every
  file's fingerprints to find a match, hit or miss. The failure mode of getting this wrong is
  silent — a second run simply reports fewer duplicates than the first, with no error.
- **Rayon `par_iter` over files remains the only parallelism unit** (ADR 0009 holds without
  modification). Phase 2 is deliberately sequential: it is memory-bandwidth-bound, not
  CPU-bound, and parallelizing over hash buckets would cost determinism for little measured
  gain.
- **Gated on run scope, off by default.** `RunOptions` gains `cross_file: bool`, defaulting to
  `false` so every existing caller and test keeps today's behavior. A duplicate that appears or
  vanishes depending on which paths the user happened to name is worse than useless, so
  path-scoped runs and the pre-commit hook (which lints a staged subset) skip the stage and
  print a note; `poly lint .` (an unscoped run) turns it on.
- **Hierarchical-config asymmetry — this amends how ADR 0018 is read for cross-file findings.**
  A clone group can span files governed by different nested `poly.toml`s, and there is no
  coherent "the" config for a pair of files. The rule: *participation* (enabled, excluded,
  normalization mode, `k`, `w`) is read from each file's own `config_id`, as ADR 0018 already
  does for every other per-file setting. *Reduce thresholds and `level`*, which apply to the
  group as a whole rather than to one file, come from the root config instead. If two
  participating configs disagree on a setting that affects the fingerprint itself, the run
  warns naming both configs and uses the root's — a silently split index would just report
  fewer duplicates than actually exist, which is the same failure mode as a missed cache
  contribution above.
- **Findings carry no `Edit`.** Duplication is not a syntax error; poly cannot extract a shared
  function for the user, so `--fix` never touches a `duplicate-code` diagnostic.
- **Reporting: one `Diagnostic` per occurrence**, with the other members of the clone group
  listed as peers in `title` / `description` / `metadata` (`metadata.group` is the stable join
  key across occurrences). Rejected alternative: a primary/secondary model, where one
  occurrence is canonical and the rest point at it. That needs a new field on `Diagnostic`,
  which propagates through `poly-mcp`'s DTOs and its `schemars`-generated JSON schema for no
  offsetting benefit. One-per-occurrence needs zero schema change, and it puts the finding on
  the file the reader is actually looking at, rather than forcing them to chase a pointer to
  find out why.

## Consequences

Positive:

- poly gains cross-file duplicate detection, which no wrapped tool provides, by reusing the
  existing per-file rayon loop and blake3 cache rather than adding a second parallel pipeline.
- The map phase costs almost nothing beyond what the runner already does per file; the
  reduce is a single, well-understood sequential pass with no new concurrency primitives.
- The scope gate (`cross_file`) keeps the feature from producing misleading results on the
  runs where it cannot be sound — a narrowed file set or a staged subset.

Negative / risks:

- **Approximate spans.** Fingerprint-aligned matches can be short by up to `w` tokens at
  either end of the true clone, since the window boundary is chosen for hash selection, not
  for readability.
- **Platform determinism.** `engines/treesitter/mod.rs` documents that the C#/Java grammars
  tokenize differently across macOS and glibc. For DRY this means a clone found on a
  developer's Mac may not be found by the same run on Linux CI — acceptable at the default
  `warning` severity, but a real hazard if a repository were to promote `duplicate-code` to
  `error` as a hard gate.
- `Namespace::Dry` is a new cache namespace with its own storage format (compact binary, not
  the `serde_json` used for `Namespace::Lint`), which is one more thing `poly cache` must
  account for in stats, gc, and size reporting.

## Alternatives considered

- **Make DRY an `Engine`:** rejected outright — `Engine::lint` is per-file and pure by
  contract; a clone-detection engine would either violate that contract silently or need a
  second, parallel notion of "engine" just for this one backend.
- **Home it in `poly-workspace`:** rejected — that crate's whole job is shelling out to
  external whole-project tools against the live worktree; it holds no AST index and adding one
  there would duplicate tree-sitter plumbing that the per-file runner already owns.
- **Store the fingerprint index in SQLite, as thai-lint does (memory / tempfile / persistent
  modes):** rejected — poly already has a persistent per-file store, the blake3 result cache,
  which gives the same incremental benefit thai-lint's "persistent mode" does through
  machinery poly already maintains. Adding SQLite would be a second storage engine for no new
  capability.
- **Primary/secondary `Diagnostic` model instead of one-per-occurrence:** rejected — see
  Decision above; the schema cost is not justified by the benefit.
