# 0033 — Batch Engine Execution for Native-Toolchain Backends

- Status: Accepted
- Date: 2026-09-16
- Updated: 2026-09-16 — implemented; see "Amendment — 2026-09-16 (as built)" below.
  The gate this ADR set on itself was measured and passed, and four decisions
  changed in contact with the tools.

## Context

ADR 0014 wraps first-party CLIs as **single-file, stdin→stdout** subprocesses, and the runner runs
the file loop as a rayon `par_iter`. The two compose cleanly and have one cost nobody measured
until now: poly pays a **process spawn per file per engine**, and for every tool in the
native-toolchain tier that spawn is most of the work.

Measured on the development host (14 logical cores, macOS 25.6, `shfmt` 3.14.1, 200 `.sh` files),
three runs:

| | run 1 | run 2 | run 3 |
| --- | ---: | ---: | ---: |
| 200 sequential per-file invocations | 1.845 s | 1.718 s | 1.557 s |
| one invocation over all 200 paths | 0.023 s | 0.060 s | 0.102 s |
| 200 × `shfmt --version` (the startup floor) | 1.738 s | 1.419 s | 1.406 s |

A file costs 7.8–9.2 ms, of which 7.0–8.7 ms is the Go runtime starting up. **Roughly 90% of what
poly spends formatting shell is spent not formatting shell.** The host was under heavy concurrent
load (1-minute load average 126) throughout, so the absolute figures are inflated; the *ratio* is
what the decision rests on, and it was stable across three runs.

This is not a shell problem. The startup floor of every tool in the tier, measured the same way on
the same loaded host (10 invocations each — upper bounds, and useful mainly for ordering):

| tool | ms / invocation |
| --- | ---: |
| `gleam` | 17.6 |
| `gofmt` | 18.0 |
| `shfmt` | 19.7 |
| `shellcheck` | 38.8 |
| `zig` | 44.8 |
| `rustfmt` | 53.8 |
| `swift-format` | 68.1 |
| `ktfmt` (JVM) | 211 |
| `dart` | 234 |
| `google-java-format` (JVM) | 316 |
| `Rscript` + `styler::style_text` on one trivial line | **982** |

`Rscript --version` alone is 10.6 ms; the 982 ms is loading the `styler` package, which poly pays
on **every R file**. That is the single largest per-file tax in the whole tier, three times the
next worst, and it is paid entirely for nothing.

Batching cannot buy parallelism *inside* the tool. `cmd/shfmt/main.go` (654 lines, upstream
checkout) contains zero `go func`, zero `chan` and zero `sync.` references, and walks with a
sequential `filepath.WalkDir`. Batching buys startup amortization and nothing else — which is
precisely the 90%.

The 2026-09-15 amendment to ADR 0014 made `shfmt` default-on, joining `gofmt`, `rustfmt` and
`shellcheck`. Four tools now run on most machines without anyone opting in, so this cost is no
longer confined to the opt-in tier.

## The constraints any design has to survive

1. **`Engine::format` is per-file by contract**, and the runner is a `par_iter` over discovered
   files with a 16 MiB worker stack.
2. **poly passes content, not paths.** `SourceFile.content` is an `Arc<str>` of bytes poly already
   read. Under the commit gate (ADR 0019), `poly fmt` / `poly lint` run with cwd set to the staged
   snapshot and receive snapshot-relative paths, so `std::fs::read` already yields staged bytes —
   but the bytes handed to an engine are *not* always on disk anywhere: pass ≥ 2 of the format
   fixed-point loop and every re-lint pass of `lint --fix` operate on in-memory content.
3. **In-place rewriting collides with the runner.** `shfmt -w` and its equivalents write files
   themselves, which fights `runner/edits.rs`'s atomic application, the `--check` dry-run default,
   and the format fixed-point loop. `-l` reports *which* files drift but not their content, so poly
   could not diff, could not apply `normalize_whitespace`, and could not converge.
4. **Per-file error attribution must survive.** A batch failure must not mark 400 files errored.
5. **The blake3 result cache is per-file.**
6. **Config is resolved per file** (ADR 0018): two files in one run can be governed by different
   configs, so a batch cannot span differing `EngineConfig`s.
7. **argv length limits.** `getconf ARG_MAX` is 1,048,576 on this host; Windows `CreateProcess`
   caps the whole command line at 32,767 characters, and `x86_64-pc-windows-msvc` is a release
   target.

## What the tools can actually do

Audited against the installed binaries' own `--help` and real invocations, not against assumption.
All eleven accept multiple paths on argv. What they do with them is where they diverge:

| engine | multi-file → stdout | delimited? | in-place | batchable, how |
| --- | --- | --- | --- | --- |
| `gofmt` | yes (default) | **no** — 2 files → 895 lines = 579 + 316, no separator | `-w` | in-place only |
| `rustfmt` | `--emit stdout` | **yes** — `<abs-path>:` + blank line before each file | default | either |
| `zig fmt` | **no** — `--stdin` is single-file | — | default | in-place only |
| `shfmt` | yes (default) | **no** — 322 lines = 204 + 118, no separator | `-w` | in-place only |
| `shellcheck` | n/a (lint) | **yes** — `--format=json1` emits a `file` field per comment | — | natively |
| `google-java-format` | yes (default) | **no** — same file twice → 16 lines = 8 + 8 | `-i`, `@ARGFILE` | in-place only |
| `ktfmt` | **no** — multi-file is in-place only | — | default, `@ARGFILE` | in-place only |
| `styler` | poly authors the `-e` script | — | `style_file(path, …)` | see below |
| `swift-format` | yes (default) | **no** — 88 lines = 44 + 44 | `-i`, `-p/--parallel` | in-place only |
| `dart format` | **yes — `-o json`**: one JSON object per file, `path` + `source` | yes | default | natively |
| `gleam format` | **no** — `--stdin` is single-file | — | default | in-place only |

Two of eleven have a native delimited multi-file *content* protocol (`rustfmt`'s path-header
stdout, `dart format -o json`). One has a native per-file *diagnostic* protocol (`shellcheck`
json1). **Eight of eleven have no usable multi-file stdout at all** and can be batched only by
letting them rewrite files.

`styler` is a special case: poly writes the driver script, so poly decides the protocol.
`styler::style_file`'s body is `path <- set_arg_paths(path); transform_files(path, …)` — vectorized,
with a `dry` parameter — so a poly-authored multi-file script is plausible. **This was inspected,
not executed; it is unverified.**

That eight-of-eleven figure is what decides the design. A contract built on parsing multi-file
stdout would serve two tools.

## Decision

### 1. An optional batch method on `Engine`, defaulting to the per-file path

```rust
/// Which operations a backend can perform over a whole batch in one call.
#[derive(Debug, Clone, Copy, Default)]
pub struct BatchSupport {
    pub format: bool,
    pub lint: bool,
}

pub trait Engine: Send + Sync {
    /// Defaults to no batching: every existing backend is untouched.
    fn batch_support(&self) -> BatchSupport { BatchSupport::default() }

    /// Format every member of `batch`. The returned vector MUST have the same
    /// length as `batch`, and element `i` MUST describe `batch[i]`.
    fn format_batch(&self, _batch: &[SourceFile], _cfg: &EngineConfig)
        -> anyhow::Result<Vec<anyhow::Result<FormatOutput>>> {
        anyhow::bail!("this backend does not batch")
    }

    fn lint_batch(&self, _batch: &[SourceFile], _cfg: &EngineConfig)
        -> anyhow::Result<Vec<anyhow::Result<Vec<Diagnostic>>>> {
        anyhow::bail!("this backend does not batch")
    }
}
```

- The input is `&[SourceFile]` — **content**, the same `Arc<str>` `format` takes today. A batch is
  never given a list of paths to go and read. `SourceFile.path` travels for naming and for config
  discovery, exactly as it does now, and is not what the tool reads.
- The output is **positionally aligned and length-checked**. A ragged answer is the one failure
  shape that would attribute one file's formatting to another; the runner asserts the length and
  treats a mismatch as a batch-mechanism failure rather than trusting a short vector.
- `Capabilities` is left alone. Batch support is orthogonal to lint/format/fix, and widening
  `Capabilities` would touch every backend's `capabilities()` for a property almost none of them
  have.
- A backend that declares `BatchSupport::default()` — every backend today — is never called here,
  so this ADR is additive by construction.

### 2. Batching is a bounded pre-pass, not something inside the per-file closure

The pipeline becomes: **read + filter** (`par_iter`, over a bounded window) → **group** → **dispatch
shards** (`par_iter`) → **finish** (`par_iter`). The read, the binary/UTF-8 guards, the
format-ignore filter, the generated-source opt-out and the hash-stamp guard all stay exactly where
they are and all run before anything is grouped, so a file poly declines never enters a batch.

**The batch identity is `(config_id, language, plan index)` — which is already `EnginePlan`
identity.** Constraint 6 is therefore satisfied by construction rather than by a new rule: an
`EnginePlan` holds exactly one resolved `EngineConfig` and one serialized-args value, so a batch
drawn from a single plan cannot span two configs, and cannot produce results that disagree with the
cache key they will be stored under. Two per-tool sub-keys widen the grouping where the tool's argv
depends on the individual file: `rustfmt` adds the resolved `--edition` (edition resolution walks to
the nearest `Cargo.toml`, so two files in one language can differ), and any spec with
`run_in_file_dir` or `rustfmt_config_flag` adds the anchoring directory.

### 3. Batching optimises pass one only

The format fixed-point loop and the `lint --fix` convergence loop are inherently per-file and stay
that way. The batch serves the **first** pass; the minority of files whose content actually changed
return to the existing per-file path for passes 2..N.

This is cheap because of a fact ADR 0014's own 2026-09-15 amendment measured: at the corrected
two-space default, 19 of 97 shell files change and **a second pass changes zero**. About 80% of
files need exactly one pass, and the batch covers all of them. It also sidesteps constraint 2's
sharpest edge — pass ≥ 2 content exists only in memory — without needing to.

A convenient property of the current registry makes the first pass simple: for Go, Rust, Zig, Java,
Kotlin, R, Swift, Dart, Gleam and Shell, the *format* plan is exactly one engine. `TyposEngine`,
`AstGrepEngine`, `UncommentEngine` and `QualityEngine` all declare `format: false`, so the
format-capable chain leaves the native tool alone. The first pass's input is the on-disk bytes poly
just read, with nothing chained ahead of it. **The contract must not depend on that** — a future
format-capable cross-cutting backend would break it — which is why the batch takes content rather
than paths even in the case where paths would currently work.

### 4. Cache first, batch second — and always persist a batch result

Each file's digest is computed and the cache probed during the read + filter pre-pass, in parallel,
exactly as it is today. **Only cache misses enter a batch.** On a warm repository the batch set is
usually empty and this ADR costs nothing.

One required amendment to the cache write policy: **results produced by a batch are always
persisted, bypassing `MIN_CACHE_DURATION`.** `should_cache_result` (`runner.rs:600`) refuses to
store anything that took under 5 ms, on the reasoning that persisting and reloading would cost more
than recomputing. Inside a batch, amortized per-file elapsed is far below that — which would make
poly refuse to cache precisely the results whose recomputation costs a process spawn, and re-batch
the whole corpus on every run. The heuristic is right for an in-process engine and exactly inverted
for a batched subprocess.

A second, free win: the format namespace keys on content alone (`single_file_digest`, no path), so a
batch can de-duplicate byte-identical members and submit each distinct content once. The lint
namespace keys on path *and* content (`single_file_digest_with_path`) and cannot.

### 5. Content, not paths: the batch root

**The adapter materializes each member's in-memory content into a scratch tree poly owns, and runs
the tool against that tree.** This is the decision the whole ADR turns on, and it is not a new
mechanism: `CatalogToolEngine::format_via_path` already writes source to a temp file, lets the tool
rewrite it in place, and reads it back
(`crates/poly-core/src/engines/catalog_tool/mod.rs:526`). Batching widens that from one file to N.

What follows from it:

- **`-w` stops being a collision and becomes the protocol.** The objection in constraint 3 is true
  only of a tool pointed at the real tree. Pointed at poly's scratch mirror, in-place rewriting is
  the *best* available protocol: it is per-file attributed by construction — one output file per
  input file, no delimiter to parse and no path-prefix line that a file's own contents could
  forge — and it is the only protocol that works for the eight tools with no multi-file stdout. The
  real tree is still written by the runner's atomic write and by nothing else, so `runner/edits.rs`,
  the `--check` dry run and the convergence loop are untouched. `poly fmt --check` remains a true
  dry run of the repository.
- **poly's read stays authoritative.** The tool sees exactly the bytes poly read and will diff
  against. There is no second read of the worktree, so no TOCTOU window between poly's read and the
  tool's — a window in which a concurrent editor's save would be silently formatted away by poly's
  subsequent atomic write. Path-based batching reintroduces that window; content-based batching
  never has it. The same property is what lets the mechanism serve in-memory content unchanged, if
  pass ≥ 2 batching is ever wanted.
- **Staged isolation is solved by not participating in it.** Under the gate, the hook runner sets
  the execution root to the snapshot and hooks receive snapshot-relative paths, so poly's own
  `std::fs::read` already yields staged bytes. Because the batch is fed those bytes rather than
  re-resolving paths, it cannot reach the worktree at all — including through a
  `[hooks] snapshot_include` symlink, which is a real symlink into the worktree that a *path-walking*
  tool pointed at the snapshot could follow and a content-fed tool cannot. ADR 0019's "one run, one
  tree" holds, because the batch's tree is poly's own copy of whichever tree the run already chose.
- **Location and permissions.** The scratch tree lives under the existing per-repo cache dir,
  `<platform-cache>/poly/<repo-key>/batch/<pid>/<engine>/`, which the cache crate's permissions
  helper already creates `0700` on Unix. That hardening exists because the staged snapshot is a copy
  of the repository's source; this tree is the same thing and inherits the same reasoning verbatim.
  It is keyed by pid so concurrent poly runs on one repo cannot collide (the result cache's
  single-writer posture is unchanged), and it is removed at the end of the run.
- **Layout: mirror repo-relative paths; never flatten.** Filenames are load-bearing. `shfmt` picks
  its shell dialect from the filename and reads EditorConfig; `google-java-format` puts the filename
  in diagnostics. Preserving basename and extension is mandatory, and preserving relative directory
  structure is what keeps a tool's own relative lookups meaningful.
- **Tools with their own config files do not batch by mirror.** `ToolSpec::config_files` is
  non-empty for `rustfmt`, `shellcheck` and `swift-format`, and `run_in_file_dir` /
  `rustfmt_config_flag` exist precisely so those tools can discover a config by walking *up* from
  the file. A mirror preserves structure below the run root and not the ancestors above it, so the
  config the tool discovers in the mirror is not the config it would discover in the repository —
  a silent fidelity regression, and the sort that shows up as "why does `poly fmt` disagree with
  `cargo fmt`". `catalog_tool` already has this bug in miniature (a flat temp dir in the system
  temp directory), which is a reason to be careful here rather than a licence. So: **`rustfmt` and
  `swift-format` stay per-file for now.** Batching them requires resolving the governing config
  once per group and passing it explicitly (`--config-path`, `--configuration`), which is a
  follow-up with its own correctness argument to make. `shellcheck` takes a different route (§6).
- **Cost.** A batch trades N process spawns for N small writes and N small reads. On the figures
  above, a write-plus-read of a few-kilobyte file should be one to two orders of magnitude cheaper
  than the 18–982 ms spawn it replaces. **That ratio is asserted, not measured**, and confirming it
  is the first thing an implementation must do — if it does not hold for the 18 ms Go tools, the
  mechanism is worth having only for the JVM and R tools.

### 6. `shellcheck` batches by real path, separately, and later

It is the exception on every axis: the only tool in the tier with a native per-file diagnostic
protocol (`--format=json1` carries a `file` field on every comment — verified on a real multi-file
run), the only one that writes nothing, and the only one whose path-awareness is a *feature* rather
than an obstacle (`source` directives, `--source-path`, `--external-sources`, `.shellcheckrc`).

Batching it by real path therefore changes what poly reports, not merely how fast. Today poly pipes
stdin, so shellcheck has no filename and `source` resolution is already inert; giving it real paths
makes sourced files newly visible, and under the gate makes *staged* sourced files visible while
untracked ones stay absent. That is a behaviour change, so it needs its own decision, and it needs
a marker folded into `Engine::version()` so that cached "no diagnostics" verdicts from the stdin era
are invalidated — exactly as `ToolSpec::default_on` was folded in for the 2026-08-29 flip, and for
the same reason: without it, a run from before the change keeps serving a verdict about an analysis
that never happened.

**Deferred.** It is the highest-value lint batch in the tier and the only batch that changes poly's
output, so it must not ride along with the format work.

### 7. Sharding: batching is a serialization point, and the shards are the parallelism

One invocation is one process on one core, and for `shfmt` — verified — there is no internal
parallelism to recover. A group is therefore sharded into
`clamp(ceil(n / MIN_BATCH), 1, rayon_threads)` invocations dispatched through the existing pool.

The arithmetic, for 485 shell files on this host's 14 threads at the measured 8.7 ms floor:
per-file spends 485 × 8.7 ms / 14 ≈ 300 ms on spawns; 14 shards spend 14 × 8.7 ms / 14 ≈ 8.7 ms.
**This is arithmetic, not a measurement.** A trustworthy parallel-scaling figure could not be
obtained on a host at load average 126 — `xargs -P1` through `-P12` over the same 200 files moved
only 4.64 s to 3.99 s, which is noise and not a result — so the end-to-end `poly fmt` number must be
measured on a quiet machine before this ADR moves past Proposed. That measurement is the gate on
accepting it.

`MIN_BATCH` is proposed at 8 and **is a guess**. The right value is per tool and derivable from the
floors above: a 316 ms JVM start repays a batch of two, an 18 ms Go start does not. `swift-format`
is the one tool that already parallelizes internally (`-p/--parallel`, verified in its help), so
batching it would buy amortization *and* parallelism; whether `dart format` does the same is
unverified.

### 8. argv limits

`getconf ARG_MAX` is 1,048,576 on this host. Linux is typically 2 MiB total with a 128 KiB
`MAX_ARG_STRLEN` per argument. **Windows is the binding constraint**: `CreateProcess` caps the whole
command line at 32,767 characters, and `x86_64-pc-windows-msvc` is one of the six release targets.
Three defences, in order of preference:

1. **Pass the shard's directory, not N paths.** `gofmt`, `shfmt`, `zig fmt`, `gleam`, `dart` and
   `swift-format -r` all accept directories (verified from their help), so a shard of any size
   collapses to a single argument and the limit stops existing. This is safe only because poly
   populated the tree and reads back only the paths it wrote. One shard, one sub-directory — never a
   shared root — so that "the tool did not change this file" and "the tool never visited this file"
   cannot differ in membership. The residual hazard is worth naming: handing over a directory lets
   the tool decide what is in it (`shfmt --detect` and its EditorConfig ignore rules, `gofmt`'s `.go`
   filter), so a file the tool silently declines comes back unchanged and reads as clean — the exact
   shape of non-coverage ADR 0031 exists to make loud, and one the per-file path does not have.
2. **Argfiles** where offered: `ktfmt @ARGFILE` and `google-java-format @<filename>`, both verified.
3. **Otherwise cap the shard** by a conservative argv byte budget — 30,000 bytes, comfortably under
   the Windows limit with room for the fixed argv — *and* by a file count, splitting into more
   shards rather than truncating. Truncating a shard is a silent coverage loss.

### 9. Error attribution: three layers, none of which can over-attribute

- **Batch-mechanism failure** — spawn failed, output unparseable, returned length ≠ input length →
  the outer `Err`. The runner **re-runs that shard through the existing per-file path** and reports
  whatever that produces. No file is ever marked errored on a batch's behalf, so a batch failure
  costs time and never correctness. This is the direct answer to constraint 4.
- **Per-file failure the tool reports** → the inner `Err` at that index, becoming the same
  format/lint error for that one path that the per-file path would have produced.
- **A tool that aborts the whole invocation because one member is malformed** is indistinguishable
  from the first case from outside, so it takes the same route: fall back per-file, where the one
  bad file isolates itself. This is why the fallback must be unconditional and cheap to reach rather
  than an error path.

One existing convention must **not** be generalized. `format_via_tool` maps a non-zero exit to
`FormatOutput::Unchanged` — "a syntax error in the source; never corrupt the file". Lifted to a
batch, that becomes "one member had a syntax error, so report all 400 files clean": a false pass, of
the same class ADR 0019 calls the most severe gate defect. A batch must never map a whole-invocation
failure to `Unchanged` for its members; it maps to the outer `Err` and the per-file fallback.

### 10. Scope and sequencing

1. The mechanism plus format batching for the in-place-capable tools with **no config files**:
   `gofmt`, `shfmt`, `zig fmt`, `ktfmt`, `google-java-format`, `dart format`, `gleam format`. Six of
   these seven are opt-in and default-off, which makes them the safest place to land the mechanism
   and — at 211, 234 and 316 ms floors — the place it pays most.
2. `styler`, which needs a new poly-authored driver script and would recover the largest single win
   in the tier (982 ms per file).
3. `catalog_tool`: the same mechanism, already written per-file, generalized.
4. `shellcheck` lint batching, separately, per §6.
5. `rustfmt` and `swift-format`, only once config discovery under a mirror has an answer.

## Consequences

Positive:

- The dominant cost of the default-on native-toolchain tier — process startup, ~90% of per-file time
  for `shfmt` on the measured corpus — is amortized across a shard instead of paid per file.
- Additive by construction: `batch_support()` defaults to nothing, so every existing backend, every
  tier-1 in-process engine and every cross-cutting backend keeps the code path it has today.
- The correctness-critical machinery is untouched. `runner/edits.rs`, the atomic write, the format
  fixed-point loop, `--check` and the whole skip/withdrawal accounting all still see one file at a
  time, because the tool writes only into poly's scratch tree.
- It reuses an existing, load-bearing mechanism rather than inventing one: `catalog_tool` has
  written-then-read-back temp files since ADR 0013.
- Batching by content rather than by path closes a TOCTOU window the per-file stdin path never had
  and a path-based batch would have opened.
- It composes with staged isolation without an isolation-specific code path.

Negative / risks:

- **Peak memory rises from O(threads) to O(batch window).** Saturating cores already raises peak
  memory because many parsers live at once; batching additionally holds every window member's
  content live. Mitigated by processing in a bounded window rather than materializing the corpus,
  but the window size is now a memory knob that did not exist.
- **A second on-disk copy of the batched files**, on top of ADR 0019's staged snapshot. Small,
  per-pid, under the cache dir, deleted at end of run — but it is another tree holding the
  repository's source, and it needs the same `0700` treatment for the same reason.
- **The pipeline gains two phases.** The per-file format function currently owns read, filters,
  cache and engine in one place; batching forces read+filter out of the per-file closure. That is a
  real restructuring of the most correctness-sensitive function in the runner.
- **A directory-passing shard lets the tool choose what it touches**, converting "the tool declined
  this file" into "unchanged" — a coverage hole ADR 0031 would otherwise surface.
- **Tools with their own config files are excluded for now**, so `rustfmt` — the second most common
  default-on tool — gets nothing from this ADR.
- **Two engines lose the `MIN_CACHE_DURATION` guard**, by design. If the batch path is ever reached
  for genuinely cheap work, poly will write cache entries it would previously have skipped.
- **A new class of bug becomes possible**: mis-aligned batch results, where file *i*'s formatting is
  attributed to file *j*. The length assertion catches ragged answers; it cannot catch a permuted
  one. Any protocol that relies on ordering rather than on per-file identity (a directory of files,
  a JSON object keyed by path) must be preferred, and a protocol that relies on stdout ordering must
  be treated as unsafe.

## What was not verified

Stated plainly, because the whole case rests on measurement:

- **The parallel-scaling figure.** The claim "sharding turns ~300 ms of spawn into ~9 ms" is
  arithmetic over a measured single-invocation floor, not an end-to-end measurement. The host was at
  load average 126 and no trustworthy figure could be taken.
- **That materializing files is cheaper than spawning processes.** Asserted from the relative cost
  of a small file write versus an 18–982 ms process start; not measured.
- **`styler` batching.** `style_file`'s body was read and is vectorized; it was never executed.
- **The in-place-into-a-scratch-mirror round trip.** No tool was actually run in `-w` mode as part of
  this ADR, deliberately — every measurement here was read-only against a live repository.
- **`dart format`'s internal parallelism**, and whether any tool other than `swift-format` has any.
- **The 485-file figures** (0.091 s batched versus 9.903 s per-file, ~124×) and the 80-file figures
  (7.4 ms per file, 5.9 ms startup floor) reported alongside this work were measured separately on a
  different corpus and are not reproduced here. The 200-file figures in the Context section are.
- **The cgo bridge's numbers** (349/349 byte-identical, +2.4 MB) come from a separate experiment and
  were not reproduced.

## Alternatives considered

- **Do nothing; keep the per-file spawn.** Rejected on the measurement. It was defensible while the
  tier was opt-in; with `gofmt`, `rustfmt`, `shellcheck` and now `shfmt` default-on, poly spends most
  of its native-toolchain time on process startup on machines whose owners never opted in. ADR 0014's
  original "subprocess spawn overhead is amortized" was an assumption, and it is wrong by roughly an
  order of magnitude.
- **A cgo FFI bridge — vendor `mvdan.cc/sh` as a C archive and link it in.** Rejected, though it is
  the most attractive option on output fidelity: the experiment reported 349 of 349 files
  byte-identical to the CLI for +2.4 MB of binary. It is rejected on distribution, not on
  correctness. It requires a Go toolchain to *build poly* on all six release targets
  (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`,
  `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`), and at least three of
  those have real friction: cgo on Windows wants mingw-w64 rather than MSVC, which leaves the MSVC
  target an unresolved question; cgo against musl is a known trouble spot; and cross-compiling
  `aarch64-unknown-linux-gnu` with cgo needs a cross C toolchain in the release image. It also buys
  exactly one tool. Batching is general across a tier of eleven plus the whole catalog tier, needs no
  new build-time system dependency, and keeps the lean-binary posture of ADR 0001 and ADR 0003.
  Worth reconsidering for `shfmt` alone if batching's measured win falls short — the two are not
  mutually exclusive.
- **Reimplement the tools in Rust.** Rejected by ADR 0014 and still rejected: a maintenance sink that
  will never match first-party behaviour.
- **A persistent tool daemon — spawn one long-lived process per tool and stream files to it.** This
  would amortize startup perfectly and preserve the per-file request/response shape, keeping every
  runner invariant. Rejected because **not one of the eleven tools has a server mode**; poly would
  have to invent a framing protocol the tools do not speak. Reconsider only for a tool that ships
  one.
- **Batch against the real worktree paths.** Rejected. It reintroduces a second read of the tree
  between poly's read and the tool's, which is a TOCTOU window in which the atomic write silently
  destroys a concurrent save; it cannot serve pass ≥ 2 or `lint --fix` content, which exists only in
  memory; and under the gate it lets a path-walking tool follow a `snapshot_include` symlink out of
  the snapshot and into the worktree, breaking ADR 0019's "one run, one tree". The performance is
  identical to the scratch-mirror approach minus the file writes — a small saving for a large class
  of correctness problem. `shellcheck` is the deliberate exception (§6), because it writes nothing,
  so the TOCTOU concern does not apply, and because path-awareness is the thing being bought.
- **Multi-file stdout parsing as the primary protocol.** Rejected on the audit. `gofmt`, `shfmt`,
  `google-java-format` and `swift-format` concatenate their outputs with **no separator whatsoever**
  — verified by line count on real files — so the result is unsplittable. `rustfmt`'s `<path>:`
  header and `dart -o json` are the only two usable protocols, and a contract built on them would
  serve two of eleven tools. They remain available as per-tool optimizations that skip the file
  writes.
- **A two-phase `-l` / `--list` design: one batched invocation to learn which files drift, then
  per-file stdin only for those.** Attractive because 80% of files are already formatted, and it was
  the first design considered. Rejected on correctness: `-l` is a *second read* of the tree whose
  answer poly would then trust to skip files entirely, so a stale or racy read produces "clean" for a
  file that is not — a false pass, and one that reaches the commit gate. A skip is exactly the
  outcome that must not be taken on unverified evidence.
- **Widen `Capabilities` with a `batch` flag instead of adding `BatchSupport`.** Rejected — batching
  is orthogonal to lint/format/fix and applies to at most one of the two operations at a time
  (`shellcheck` batches lint and does not format; `shfmt` the reverse), so a single bool would be
  ambiguous, and widening `Capabilities` touches every backend for a property almost none of them
  have.
- **Batch inside the rayon closure — let each worker accumulate files and flush.** Rejected: workers
  would need shared mutable accumulators and a flush barrier, reintroducing exactly the contention
  the per-file model avoids by keeping files independent, and the resulting batch membership would be
  nondeterministic — which makes a mis-attribution bug unreproducible.
- **Raise `MIN_CACHE_DURATION`'s threshold rather than bypassing it for batches.** Rejected — the
  threshold is correct for in-process engines, where recomputation genuinely is cheaper than a disk
  round trip. The distinction is not how long the work took but whether repeating it costs a process
  spawn, so the exemption belongs to the batch path rather than to the constant.

## Amendment — 2026-09-16 (as built, and a partly negative result)

Implemented — and the headline premise of this ADR did **not** survive
measurement. The mechanism works, produces byte-identical output, and is a large
win for one class of tool. For the class this ADR was written about, it is a
**regression**, and it ships disabled there.

### What the Context section got wrong

The Context section argues from "~90% of per-file time is startup". That figure
is real but it is a **per-file, serial** statistic, and poly's runner is not
serial — it is a rayon `par_iter` across every core. Startup is therefore already
overlapped before batching does anything, so the share of *wall-clock* it can
recover is far smaller than 90%.

Worse, the comparison is not like-for-like. The per-file path pipes content over
**stdin**: the tool opens no files at all. A batch necessarily adds file I/O —
poly writes a scratch mirror, the tool reads and rewrites it, poly reads it back.
That is new work in exchange for a saved spawn, and for a tool that starts
quickly the trade is bad.

Measured end-to-end, `poly fmt --check --no-cache` over 400 real shell files,
same binary, batching the only variable (three runs each):

| | wall |
| --- | ---: |
| per-file | 0.39 / 0.49 / 0.50 s |
| batched | 1.93 / 2.00 / 2.27 s |

**Batching `shfmt` is roughly 4x slower.** CPU accounting shows exactly the
predicted trade: user time falls 1.01 s -> 0.67 s (the saved Go-runtime starts)
while system time rises 1.85 s -> 2.30 s (the mirror's syscalls).

### What is nonetheless true

- **Correctness is unaffected.** Output is byte-identical across 400 files, and
  the one file that differs from a single `shfmt -i 2` pass differs because
  poly's fixed-point loop runs a second pass and reaches `shfmt`'s own fixed
  point — pre-existing behaviour, and a direct demonstration that pass 2
  correctly falls through to the per-file path.
- **The mechanism does what it claims.** Instrumented spawn counting over the
  same corpus: **684 -> 413** invocations, exactly the 285 pass-one spawns
  replaced by 14 shard spawns. The shards are genuinely concurrent (14 starts
  within 148 ms).
- **It is a large win where startup dominates.** 60 files, `xargs -P14`
  per-file against the same files in shards. Read these as indicative only: the
  `xargs` per-file arm re-reads each file, whereas poly's real per-file path
  pipes content over stdin and reads nothing — so these ratios **flatter
  batching** relative to what poly actually does.

  | tool | startup | per-file | batched | speedup |
  | --- | ---: | ---: | ---: | ---: |
  | `ktfmt` | 211 ms | 10.82 s | 1.69 s | **6.4x** |
  | `google-java-format` | 316 ms | 3.33 s | 0.93 s | **3.6x** |
  | `zig fmt` | 45 ms | 0.71 s | 0.25 s | 2.9x |
  | `gofmt` | 18 ms | 0.16 s | 0.05 s | 3.2x (synthetic) |
  | `shfmt` | 20 ms | 0.35 s | 0.34 s | none |

### The decision that follows: opt-in, per tool

**Batching is off unless a tool's config asks for it** —
`[fmt.<lang>.<tool>] batch = true`. It is implemented for `gofmt`, `shfmt`,
`zig fmt`, `ktfmt`, `google-java-format`, `dart format` and `gleam format`, and
inert until switched on.

A hardcoded default was tried first — gate on the tool's startup cost — and
abandoned, because the data does not support any fixed rule:

- **Startup does not predict the outcome.** `gofmt` and `shfmt` both start in
  ~20 ms. On the same machine, `gofmt` measured 3x *faster* batched and `shfmt`
  4x *slower*.
- **Neither does the tool.** Re-measured later at load average ~200, `shfmt`
  measured 4-7x **faster** batched — the opposite sign from the same tool on the
  same machine hours earlier.

Both observations have the same cause. Batching does not remove work, it
*substitutes* work: one process spawn for a mirror write, a tool-side read and
write, and a read back. Which side is cheaper depends on file sizes and on how
contended the machine is. On a busy machine spawns are expensive and batching
wins everywhere; on an idle one the runner's `par_iter` already overlaps startup
across cores and the added I/O dominates.

There is no default that is right in every environment, so poly does not pick
one. The measurements below say where to look; the config key says what to do
about it.

`rustfmt`, `swift-format` and `styler` cannot be switched on at all. Their
exclusion is about **correctness**, not speed: the first two discover config by
walking up from the file, which a scratch mirror does not reproduce, and
`styler`'s argv is a poly-authored `Rscript -e` program taking a single path.

### 1. A malformed member does **not** abort the batch — and that is the danger

Section 9 anticipated "a tool that aborts the whole invocation because one member
is malformed". Neither `shfmt` nor `gofmt` does that. Given a directory
containing one unparseable file, both **formatted every valid file, left the
broken one byte-identical, and exited non-zero**.

That is worse than aborting, because it makes the exit code the *only* usable
signal. A tool rejecting a bad **flag** also exits non-zero — having done
nothing, leaving every file byte-identical. After the fact those two states are
indistinguishable from the files alone, and reading them back in the second case
reports every file "unchanged", i.e. **clean**: a false pass, reached without any
formatter having looked at the source.

**As built: any non-zero exit discards the entire batch and falls back to the
per-file path.** This has a consequence worth stating, because it was observed:
the larger the shard, the likelier it contains a malformed file, and the more
work one bad file discards. With every candidate in a single shard the entire
prefill was lost. The fallback is still correct — those files are simply
formatted per-file — but the batch work is wasted, so shard size is a
reliability parameter and not only a parallelism one.

### 2. Explicit paths, not directories — section 8's first defence is withdrawn

Section 8 preferred passing the shard's *directory* so argv limits stop existing,
while naming the hazard: the tool then decides what is in it, so a file it
silently declines "comes back unchanged and reads as clean". That hazard is the
same false pass as above and is not worth an argv optimisation.

**As built: every file is named explicitly**, split into as many invocations as
the 30,000-byte budget requires (section 8's third defence). The budget never
truncates, it only splits.

### 3. A prefill, not a restructuring

Section 2 proposed hoisting read-and-filter out of the per-file closure. As
built, `format_one` is **not restructured**. The batch runs as a pre-pass that
computes what pass one would produce and hands the results to the loop as a
lookup table (`runner/batch.rs`).

This makes both error directions harmless by construction: over-inclusion
(batching a file the loop later skips) leaves an entry nobody reads, and
under-inclusion leaves the file on the per-file path. Every filter — binary,
UTF-8, format-ignored, withdrawn, generated, hash-stamped — stays in one place,
in one order, unchanged. Batched files are **read twice**, which is part of why
the trade is bad for cheap tools, but it buys the property that a bug in this
module cannot change *which* files the run checks.

A lookup is honoured only when the entry's stored input equals the content in
hand. That confines the prefill to pass one and makes a permuted or stale result
impossible to apply to the wrong file — the "new class of bug" the Consequences
section warns about, closed by comparison rather than by ordering discipline.

### 4. Scratch tree: `tempfile`, not the cache directory

Section 5 placed the mirror under `<platform-cache>/poly/<repo-key>/batch/<pid>/`.
As built it is a `tempfile::TempDir`: created `0700` on Unix — the property
section 5 wanted — and **removed on drop, including on panic and early return**,
which a pid-named cache subdirectory is not. Section 5's objection to
`catalog_tool`'s temp dir is about *flatness* losing config discovery, which does
not apply here because every batched tool has an empty `config_files`.

### Also as built

- **Memory is bounded.** `MAX_PREFILL_BYTES` (64 MiB) caps what the prefill holds
  live; past it the remaining files take the per-file path. Candidates are sorted
  by path first, so which files get batched is deterministic rather than a
  function of the order rayon finished them in. Unchanged files cost their
  content once — an entry's input and output are two handles on one `Arc`.
- **The `MIN_CACHE_DURATION` bypass landed as specified** (section 4).
- **`Engine::batch_support` carries only `format`.** The `lint` half was dropped
  until `shellcheck` is actually decided (section 6): declaring a field nothing
  consults would suggest setting it does something.
- **EditorConfig does not leak in.** Giving `shfmt` a real path normally enables
  EditorConfig lookup, which stdin mode does not do — but an explicit `-i` (which
  poly always passes) overrides it, and the mirror lives in a temp directory
  where no project config is reachable. Verified rather than assumed.

### Still not verified

- **Nothing here was measured on an idle machine.** Ratios only.
- `dart format`'s batching is enabled on its 234 ms startup, by analogy with the
  two JVM tools; it was not itself benchmarked batched-vs-per-file.
- `styler` (982 ms startup, the largest prize in the tier) is still unimplemented.
