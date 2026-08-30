---
priority: medium
description: "poly's coverage tiers (native / tree-sitter / native-toolchain / quality / built-in ast-grep pack), the per-file tier vs the whole-project phase (--no-workspace), staged isolation (ADR 0019), and hierarchical poly.toml in monorepos (ADR 0018)"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:ecc6b9fd72aa7b536a36d3bcf2b185dc98db5bf4851d5297808f9006db9f77ca
Source-Hash: blake3:261d9152e15dfdc66715216442e953f184ed7b10aacc8dd544b41b8dc501ec75
Schema-Version: v1
-->

# poly Tiers and Scope

## Coverage tiers inside the per-file pass

Five mechanisms decide what actually inspects a file (`crates/poly-core/src/registry.rs`):

1. **Tier-1 native crate backends** — one in-process Rust crate registered for specific
   languages: ruff, oxc, taplo, rumdl, sqruff, yaml, malva (+ biome for CSS/SCSS), markup_fmt, mago, rubyfmt,
   graphql, nixfmt, hcl, dockerfile, dotenv, ini.
2. **Tier-2 tree-sitter generic tier** (ADR 0004) — the catch-all for every language with no
   native backend. Structural reindent for brace-family grammars, whitespace normalization
   otherwise, and a leave-untouched list where whitespace is significant. This is the
   coverage mechanism, not a fallback to avoid — but it is explicitly *not* gofmt/rustfmt
   parity.
3. **Native-toolchain tier** (ADR 0014, `engines/native_tool/`) — the language's canonical
   first-party CLI, run per file over stdin/stdout. `rustfmt`, `gofmt` and `shellcheck` are
   **on by default whenever the tool is found on PATH**; `zig fmt`, `shfmt`,
   `google-java-format`, `ktfmt`, `styler`, `swift-format`, `dart format`, and
   `gleam format` are opt-in via `[fmt.<lang>.<tool>] enabled = true` /
   `[lint.<lang>.<tool>] enabled = true`. Absent tool ⇒ the language falls through to
   tier 2 with an info-level notice, never an error.
4. **`quality`, the code-quality metric tier** (ADR 0027) — cross-cutting, appended to every
   language: file/function/type length, nesting depth, cyclomatic complexity, parameter
   count, and `lazy-ignore`. Warning severity, on by default, configured under
   `[lint.quality]`. The structural rules need a verified node-kind table and cover Python,
   Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C#, and Ruby; `magic-number`
   and `law-of-demeter` are opt-in. A per-language deferral table keeps it from duplicating
   a tier-1 rule (ruff's `C901`/`PLR0913`, oxlint's `max-depth`/`max-params`).
5. **The built-in ast-grep rule pack** (ADR 0029) — 26 rules across 9 languages (C#, Elixir,
   Go, Java, Kotlin, Python, Ruby, Rust, Swift) embedded in the binary and on by default; 13
   of them ship `severity: off`. It sits *beneath* `[rules] dirs` (default `.poly/rules`), so
   a user rule with the same `id` replaces a pack rule. `[rules] builtin = false` disables
   the pack wholesale.

`typos` (spelling) and the opt-in `uncomment` engine are the other two cross-cutting
backends appended to every language.

## Per-file tier vs whole-project phase

`poly lint` runs in two phases:

- **Per-file tier** — the parallel, cached engine pass over every discovered file, using the
  five mechanisms above. This is all `--no-workspace` runs.
- **Whole-project phase** — invokes the same whole-workspace tools a pre-commit hook would
  (`cargo clippy` / `cargo-sort` / `cargo-machete` / `cargo-deny`, plus any inline
  `workspace = true` job such as a type checker) on the live worktree, folding their
  pass/fail into the report and exit code. It reuses the `[hooks]` config as its single
  source of truth. On by default; disable with `--no-workspace` or `[lint] workspace =
  false`. A repo with no `[hooks]` section has no whole-workspace hooks to lower, so it runs
  only the per-file tier.

**Plain `poly lint` is not read-only.** poly applies no fixes without `--fix`, but the
whole-project phase *executes* those tools against the live worktree, and their own side
effects are not poly's to control (`cargo clippy` populating `target/` or refreshing
`Cargo.lock`, a `go` invocation appending to `go.work.sum`, a type checker writing its
cache). Only `--no-workspace` / `[lint] workspace = false` gives a run that cannot touch the
tree.

The phase is also **not path-scoped**: `poly lint <paths>` skips it when the paths *narrow*
the run, and `--workspace` opts back in with the tools covering the whole repository —
regardless of the named paths and of `[discovery] exclude`, which filters poly's own
discovery only. Naming the directory poly is running in (`poly lint .`) is a request for the
whole project, not a narrowing, so the phase still runs. A note on stderr says which branch
was taken.

Under `--fix`, the whole-project phase runs the tools in fix mode (`cargo sort` in place,
`cargo-machete --fix`, `cargo clippy --fix --allow-dirty --allow-staged`; `cargo deny` stays
check-only). Fix mode is what `--fix` adds — the phase runs either way, so `--no-workspace`,
not the absence of `--fix`, is what skips it. The git-hook / commit-gate path never requests
fix mode. `poly fmt` never runs this phase.

## Staged isolation (ADR 0019)

On the commit-gate path, poly materializes a snapshot of the git **index** (via
`git checkout-index`) and runs the stage from it, so unstaged edits neither hide nor cause
failures. Details that matter:

- The snapshot is the execution root for **every** hook in the run, not just the
  whole-workspace ones — a gate whose per-file hooks read the worktree while its
  whole-project hooks read the index would report on two different sets of bytes.
- Isolation is active only for the index stages (`pre-commit`, `pre-merge-commit`) and is
  skipped for `--all-files` (which deliberately checks the whole tree) and for non-index
  stages such as `pre-push`. `[hooks] isolate = false` forces it off.
- The snapshot lives in the per-user OS cache dir (`~/.cache/poly/<repo-key>/staged`), not
  in-repo, and is refreshed incrementally by index OID so tool caches stay warm.
- `poly lint`'s whole-project phase is a *different* path: it runs against the live
  worktree, so no snapshot is built.

## Hierarchical poly.toml in monorepos (ADR 0018)

Config resolves hierarchically: every `poly.toml` from the repository root down to the file
being processed is deep-merged, so a nested config layers on top of its ancestors and each
package can tune rules while sharing a root baseline. The upward walk is bounded at the
`.git` directory (or an explicit `[workspace] root = true`), so a stray `poly.toml` in
`$HOME` is never picked up. `poly.local.toml` remains the final local override layer, and a
top-level `extends` list (ADR 0020) shares sections from local or pinned-remote base
configs — `poly config update` pins a symbolic remote ref into `poly-config.lock`, and
`poly config show` prints the effective merged config.
