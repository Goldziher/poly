# 0035 -- Recipe Tools: Batched, Grouped, Report-Parsing External Tools

- Status: Accepted
- Date: 2026-10-28

## Context

ADR 0013 made every tool configurable from `poly.toml` through a vendored mdsf catalog, and ADR 0033
batched the native-toolchain backends. The catalog tier itself kept ADR 0013's model: one spawn per
file, the exit code is the verdict, a failure is one span-less diagnostic. Measured on the armis
monorepo (92k files) that model fails for exactly the tools a pre-commit gate runs:

| Tool | Before |
| --- | --- |
| `buf format` | one spawn per file, 101 spawns / 49.5 s for 100 protos |
| `buf lint`, `golangci-lint run`, `tofu fmt`, `golangci-lint fmt` | unreachable: the catalog names the command `lint`/`run`/`fmt`, which poly never selected |
| `buildifier` | unreachable: BUILD files were not recognised as any language |
| `actionlint`, `hadolint` | one spawn per file; one blob of text per file, no line, column or rule; no way to pass `-ignore` |
| `checkstyle`, `palantir-java-format`, `helm`, `tflint`, `pyproject-fmt` | absent |

The tools differ in the unit they work on. checkstyle and actionlint take any list of files; `helm lint`
takes a chart directory; `golangci-lint` reports false `undefined` errors when handed a lone file and
needs its package; `buf` needs its module. A per-file engine cannot express that, and the hook runner's
file-list lowering (`stages`) batches but cannot parse a report or dedupe a unit.

## Decision

Extend the catalog, not a parallel mechanism.

- **Recipe.** `poly-catalog` gains `data/overlay.json`, poly-owned data layered over the mdsf snapshot
  (commands merge over the mdsf ones so the golden fixtures stay valid). A tool entry may carry a
  `recipe`: the report format (`output`), how files group into invocations (`group`: none, directory,
  or the nearest ancestor holding a marker file such as `Chart.yaml`, `go.mod`, `buf.yaml`), what an
  invocation is pointed at (`target`: the files, the root, or the files' directories), which findings
  are reported (`findings`: own, or all), the files whose content decides a verdict
  (`dependencies`), the config files it reads, and pinned releases (`install`).
- **Argv templates.** Commands use `$FILES`, `$FILES:--path`, `$DIR` and `$ROOT`. `$FILES` is
  sharded to a 30,000-character budget; an embedded `$DIR` (`--chdir=$DIR`) means one invocation per
  directory; `then` chains in-place passes (palantir's two); `batch` gives the many-file form of a
  command whose primary form reads stdin.
- **Engine.** `CatalogToolEngine` delegates to a `RecipeTool` when the catalog entry has a recipe.
  It implements `lint_batch` / `format_batch`, so the existing runner prefill (ADR 0033) drives it.
  Formatting runs in place on a scratch copy that keeps relative paths and copies the config and
  marker files beside it (a tool judges a file by its name and its directories). Linting runs on the
  real files when poly's bytes match disk, on a scratch copy otherwise. A failing invocation is
  halved until the failure is pinned on single files; a tool that refuses two single files before
  accepting anything is abandoned instead of halved into a spawn per file. A non-zero exit that
  reports nothing is a failure, never a clean file.
- **Units and parallelism.** Groups run in parallel on the runner's pool, one at a time for a tool
  marked `serial` (golangci-lint's cache lock). Identical invocations are memoized for the process,
  so one chart asked about from several shards runs once. A group is never split across shards
  (`Engine::batch_shard_floor`); a JVM tool raises the floor instead, because a second JVM costs
  more than it saves.
- **Cache.** `Engine::lint_cache_salt` folds a digest of a unit's `dependencies` into the file's
  lint key. A tool whose verdict depends on which other files were checked (`findings = all`) is
  keyed to the process and recomputed, because no per-file key can express it.
- **Reports.** One parser per format, all strict: output that is not the expected document is an
  error, never "no findings". Severities follow the tool's own where it has them.
- **Fixes.** A finding the tool marks auto-fixable carries the tool's own rewrite of the file as a
  whole-file edit, produced by its fix command on a scratch copy.
- **Takeover.** A recipe formatter enabled by name replaces the language's built-in formatter; one
  confined to named files (`pyproject-fmt`) supplements it, and the built-in is wrapped
  (`FormatScope`) so it leaves those files alone. Chaining two formatters over one file never
  converges. A missing tool takes over anyway and its files are skipped with a reason: falling back
  to the built-in would rewrite files the project hands to a specific formatter.
- **Provisioning.** `[tools.<name>] bin`, `version`, `url`, `sha256`, `install = "auto"`. The
  catalog carries checksummed releases (checkstyle jar, palantir native image, buildifier);
  downloads are verified before they are made executable and stamped so later runs do not rehash.
  poly never downloads unless `install = "auto"`; a missing tool otherwise yields a warning naming
  the command that installs it. JVM tools launch as `java -jar` with `bin` / `JAVA_HOME` / `PATH`.
- **Config.** `[tools.<name>]` gains `extra_args`, `format_extra_args`, `lint_extra_args` (the
  catalog's own flags are kept, so the report parser still works), `bin`, `version`, `url`,
  `sha256`, `install`. `files` / `exclude` now apply on the direct path as well as the hooks path.
- **Migration.** `poly migrate` maps the lefthook / pre-commit jobs that run these tools, and the
  wrapper scripts that launch them, onto `[tools]` with their flags; it reads `.checkstyle-version`
  and `.palantir-java-format-version` into `version`. Behaviour the wrapper had that poly does not
  reproduce is listed as lossy.

## Consequences

Positive: one spawn per shard instead of per file; real file/line/rule diagnostics; the armis gate
tools run from `poly.toml`; parity with the original wrappers measured on the armis corpus.

Negative: the overlay is hand-maintained data (its checksums are verified by a catalog test);
`helm` cannot run `helm dependency build`, and neither `helm template` values selection nor the
UNRENDERABLE list is reproduced; `golangci-lint` reports every finding in a package rather than only
new ones unless `--new-from-rev` is passed through `lint_extra_args`.

## Alternatives considered

- **Lower `[tools]` entries to hook file-list jobs only.** Batches, but cannot parse a report, dedupe
  a unit, cache per file, or apply `--fix`.
- **A new engine trait for unit-scoped tools.** The batch API plus a cache salt expresses the same
  thing without a second path through the runner.
- **A daemon per JVM tool.** Not needed once a shard is one JVM; revisit if a repository's
  pre-commit set is routinely a handful of Java files.
