# 0010 — Distribution, Naming, and Pre-Commit Integration

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-07 (v0.9.0): the "poly alignment" release — a single `poly` binary (no
  `polylint` / `polyfmt` binaries), every crate renamed to the `poly-` prefix, the repo
  renamed to `github.com/Goldziher/poly`, and the npm / PyPI wrapper packages removed. See
  the update notes inline below.

## Context

The mission (ADR 0001) only lands if adoption is frictionless: discoverable crate names, a
clean separation between the reusable engine and the user-facing binaries, and a
pre-commit integration that actually deletes a repo's existing hook sprawl rather than
adding to it.

## Decision

- **Two binary crates: `polylint` (lint) and `polyfmt` (format)**, each a thin CLI over a
  shared **`polylint-core`** engine library (the `Engine` trait, registry, config,
  defaults, cache, runner, reporter). Core is a path dependency; publishing it separately
  is an open follow-up.

  > **Update (2026-07, v0.9.0):** collapsed into a **single `poly` binary** with `lint` /
  > `fmt` (and `hooks` / `commit` / `rules` / `cache` / `mcp` / `migrate`) subcommands (ADR
  > 0011); there are no `polylint` / `polyfmt` binaries. The engine library was renamed
  > `poly-core` (lib name `poly_core`), and every workspace crate now carries the `poly-`
  > prefix.
- **Names reserved on crates.io at v0.0.1** (done 2026-06-26) so the identity is locked
  before real releases, though we do not publish to crates.io (binaries are distributed as
  prebuilt artifacts, ADR 0003).
- A Cargo **workspace** with `crates/poly-core`, `crates/poly-cli`, `crates/poly-config`,
  `crates/poly-cache`, `crates/poly-catalog`, `crates/poly-hooks`, `crates/poly-mcp` (plus
  `gitfluff` and `conformance`) — every member on the `poly-` prefix.
- ~~**Ship a `.pre-commit-hooks.yaml`** defining exactly two hooks (`polylint` and
  `polyfmt`) so a consuming repo can **replace its entire hook list with two entries**
  pointed at this repo.~~ **Superseded (2026-06-29) by ADR 0011/0012:** poly ships a
  self-contained `poly hooks` runner driven by `poly.toml [hooks]`, so the
  `.pre-commit-hooks.yaml` artifact was dropped — a consuming repo wires poly directly
  instead of routing through pre-commit/prek.
- **Distribution channels.** Prebuilt release binaries (`poly-<version>-<triple>`) attached
  to GitHub releases, plus the `curl … | sh` installer, the PowerShell installer, the GitHub
  Action (`Goldziher/poly@v0`), and Homebrew (`brew install Goldziher/tap/poly`). Not
  published to crates.io.

  > **Update (2026-07, v0.9.0):** the **npm (`@nhirschfeld/polylint`) and PyPI (`polylint`)
  > wrapper packages were removed** — reversing the earlier decision to ship them — leaving
  > the installers, GitHub Action, and Homebrew as the distribution surface. The tap repo
  > keeps its Homebrew-convention name `homebrew-tap`, but the formula is now `poly` (class
  > `Poly`). The GitHub repository was renamed `Goldziher/polylint` → `Goldziher/poly`.

## Consequences

Positive:

- Clear separation of concerns: all logic lives in `poly-core`; the `poly` binary stays
  trivial and the engine is reusable/embeddable.
- A stable install story via the installers, Homebrew, and the GitHub Action (poly is not
  published to crates.io, so there is no `cargo install`).
- The headline pitch is now delivered by `poly hooks` (ADR 0012): a repo's hook sprawl
  collapses to one `poly.toml [hooks]` block driven by poly's own runner, deleting every
  per-tool hook and its system dependency — no pre-commit/prek dependency in between.

Negative / risks:

- The single `poly` binary links every backend (oxc, ruff, …), so distribution size and
  build time are dominated by the heavy engines; `poly-core` is structured so the binary
  pulls only what it needs (see ADR 0015 on the fat-CLI trade-off).
- Keeping the subcommands' UX consistent (flags, exit codes, output formats) is ongoing work.
- Whether `poly-core` is ever published (vs. path-only) remains undecided; consumers
  wanting to embed the engine are blocked until then.

## Alternatives considered

- **Single binary with `lint`/`format` subcommands:** originally rejected in favor of two
  binaries mapping onto two pre-commit hooks. **Reversed (2026-07, v0.9.0):** this is now
  the shipped design — one `poly` binary with subcommands (ADR 0011) — since the umbrella
  family (hooks, commit) made a single entrypoint the natural home.
- **No shared core (duplicate logic per binary):** rejected — guarantees drift and double
  maintenance.
- **Distribute only via pre-commit, no release binaries:** rejected — the installers,
  Homebrew, and the GitHub Action matter for direct/editor/CI use beyond pre-commit.
