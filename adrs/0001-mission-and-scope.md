# 0001 — Mission and Scope: Two Binaries Replace the Toolchain

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-07 (v0.9.0): the two-binary framing was consolidated into a single `poly`
  binary with `lint` / `fmt` subcommands (ADR 0011). `polylint` / `polyfmt` were only ever
  hook IDs, never shipped binaries; the sections below record the original intent.

## Context

A modern polyglot repository must install and pin a sprawl of per-language linters and
formatters, each with its own runtime: ruff (+Python), oxlint/oxfmt/prettier (+Node),
shfmt + shellcheck (+Go), hadolint, clang-format, ktfmt (+JVM), rubocop (+Ruby),
php-cs-fixer (+PHP), mix format (+Elixir), golangci-lint, taplo, rumdl, sqruff, typos, and
more. Each is wired separately into `.pre-commit-config.yaml`. The result is slow setup,
version drift, CI images bloated with language runtimes, and constant breakage when a
system dependency is missing or mismatched.

## Decision

Build **polylint** (lint) and **polyfmt** (format): two self-contained binaries, driven by
one config, that replace the entire per-language linter/formatter stack and eliminate
system dependencies. The mission is explicitly to _delete the toolchain_, not to add one
more tool beside it.

The **v1 target language set is the xberg-io stack** — the languages actually used across
the xberg-io repositories (Python, JS/TS, JSON, YAML, CSS, TOML, Markdown, SQL, shell, Go,
Java, Kotlin, Ruby, PHP, Elixir, C/C++, Dockerfile, Rust, proto, …). Those repos double as
the dry-run test corpus. v1 must cover that whole set, which is why tree-sitter tier-2
coverage (see ADR 0004) is in v1, not deferred.

## Consequences

Positive:

- One install, one config, zero language runtimes to provision in dev or CI.
- A repo's pre-commit config collapses to two hooks (see ADR 0010).
- Reproducible behavior: the same binary lints/formats everywhere.

Negative / risks:

- We take on the maintenance surface of many tools at once; upstream churn is now our
  problem to track and re-absorb.
- "Universal" sets a high bar — partial language coverage or lower fidelity than a
  native tool will be judged against the tools we replace.
- Scope is broad; disciplined tiering (ADR 0004) and an honest fidelity story
  (ADR 0007) are what keep it tractable.

## Alternatives considered

- **A meta-runner that shells out to existing tools** (à la pre-commit, mega-linter):
  rejected — it keeps every system dependency, the exact problem we exist to remove.
- **A single combined binary** doing both lint and format: rejected — separate `lint`
  and `format` verbs map to distinct pre-commit hooks and exit-code semantics, and keep
  each binary's CLI focused.
- **Narrower v1 (a few popular languages)**: rejected — the value proposition is
  replacing the _whole_ stack for a real repo; the xberg-io set defines "whole".
