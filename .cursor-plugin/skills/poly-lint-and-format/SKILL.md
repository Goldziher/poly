---
priority: medium
description: "Running poly lint / poly fmt — --fix, --format pretty|json|toon, --exclude, --config, exit codes, inline suppression, and the check → read-json → fix → re-check loop"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:d208bf5eb2480ca8f2fe2cafa779945b1dd2ca529962d3f1bdebf1f32ee1c94d
Source-Hash: blake3:ebf7b5b943d6520ecdcd09cd009836c9a016f0a4fff807c44b0b40cd7370b62d
Schema-Version: v1
-->

# poly Lint and Format

## Commands

- `poly lint [PATHS]…` — run the linters. poly applies no fixes without `--fix`; `--fix`
  applies autofixes (and the whole-project fix phase). `--no-workspace` restricts to the
  per-file tier — and is also what makes a run read-only: without it, plain `poly lint`
  still *executes* the configured whole-project tools against the live worktree, and their
  own side effects (a refreshed lock file, a populated build or type-checker cache) are not
  poly's to control. Naming paths that **narrow** the run skips that phase by default
  (`poly lint .` names the repo root, so it does *not* narrow and the phase still runs);
  `--workspace` opts back in, and the tools then cover the whole repository regardless of
  the named paths and of `[discovery] exclude`.
- `poly fmt [PATHS]…` — **dry run by default**: reports what would change and writes
  nothing. `--fix` writes changes; `--check` is the explicit form of the default dry run
  and conflicts with `--fix`. `poly fmt` is a pure formatter — it never runs the
  whole-project lint phase.

## Flags

- `--format pretty|json|toon` — `pretty` (the default) is the colored, human-oriented
  output; `json` and the compact `toon` variant are machine-readable. Under
  `--format json`/`toon`, `poly lint`'s whole-project section and every note (discovery,
  skips, errors) go to stderr so stdout stays a single valid document — a machine consumer
  must check the **exit code**, not just the payload.
- `--exclude <glob>` — skip paths on top of `.gitignore` (repeatable; merged with
  `[discovery] exclude`). Gitignore-style: a glob without a leading `/` matches a directory
  of that name at **any** depth (`e2e/**` also prunes `src/test/java/io/xberg/e2e/`), while
  a leading `/` anchors it to the config directory (`/e2e/**`). `poly doctor` warns when a
  rule matches at more than one depth.
- `--include-excluded` — check explicitly named files or directory roots even when they
  match the exclude set. Exclusions *below* an included directory stay active. Applying the
  exclude set to explicitly named paths is already the default, so `--force-exclude` is
  accepted only as a compatibility no-op — the flag is parsed and never read.
- `--config <path>` — point at a specific `poly.toml`.
- `--no-cache` — bypass the blake3 content-hash result cache.
- `-j <N>` — parallelism; `--no-color` — plain output; `--verbose` — extra per-finding
  detail in `pretty` output; `--debug` — per-engine cache hit/miss and timing, plus
  debug-level logs on stderr.
- `--fix-generated` — let `--fix` rewrite files marked `DO NOT EDIT` / `@generated`, which
  it otherwise reports on but leaves alone.
- `--deny-skips` / `--max-skips <N>` — strict coverage. A **skipped** file is one nothing
  inspected: a path named on the command line that no engine covers (`App.csproj`), or a
  file every routed backend declined (Go-templated YAML, a hash-stamped generated file). A
  skip the caller instructed — `--only`/`--skip` narrowing an engine out, or `enabled = false`
  in config — is **not** charged against the budget, since it names itself in the report
  rather than losing coverage silently; only a poly limitation counts. Skips are always
  reported and named; these flags make a chargeable skip fail the run (exit `2`), naming
  every file it fired on. `--verbose` lists every skip in `pretty` output; `--format
  json`/`toon` carries the full set both as a top-level `skipped` array and as synthetic
  `results` entries — `summary.skipped` is the authoritative count, so read it first and use
  the set only to name the files.

## Exit codes

- `0` — clean (no findings, no drift).
- `1` — error-severity findings, formatting drift, or a failing whole-project tool.
- `2` — an error (bad config, tool failure), or work the run could not verify: a missing
  path argument, a file an engine failed on — including a `poly fmt` file that could not
  reach a fixed point within its five-pass cap, now reported as an error rather than
  silently claimed as formatted — or a `--deny-skips`/`--max-skips` breach.

`poly lint` exits non-zero only on **error-severity** findings; warnings do not fail CI —
which is why the on-by-default `quality` and built-in ast-grep rules (all warnings) never
turn a green pipeline red on their own.

## Inline suppression

Silence one finding in place with a directive written in the host language's own comment
syntax:

```rust
let value = map.get(key).unwrap(); // poly: allow[unwrap-used] key is validated above
```

- `poly: allow[RULE] reason` — a comment-only line suppresses the next non-blank line; a
  trailing comment suppresses its own line.
- `poly: allow-file[RULE] reason` — suppresses the whole file wherever it appears, and is
  the only form that can suppress a diagnostic with no span.
- `[RULE]` accepts a comma-separated list, or `*` for every rule.
- **A reason is mandatory.** A directive whose text after `]` has no alphanumeric character
  does **not** suppress, and reports `lazy-ignore` instead — an empty comment cannot buy a
  bypass.
- Applied centrally in the runner, so it works for every engine (ruff, oxlint, quality,
  ast-grep, …). For a whole file or a class of files, use `[per-file-ignores]` in
  `poly.toml` instead.

## The loop

1. `poly fmt --check . --format json` and `poly lint . --format json` — capture drift and
   findings, checking the exit code.
2. Read the JSON to see exactly which files and rules are involved.
3. `poly fmt --fix .` then `poly lint --fix .` to apply what is auto-fixable.
4. Re-run the checks; hand-fix whatever remains (exit code back to 0).
