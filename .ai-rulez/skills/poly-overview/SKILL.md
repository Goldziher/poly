---
priority: medium
description: "What poly is — tier-1 in-process backends vs the tree-sitter generic tier, language coverage, and when to reach for it"
---

# poly Overview

poly is a single pure-Rust binary that lints and formats a whole repository across
languages. It wraps best-in-class tools as in-process crate backends and falls back to a
tree-sitter generic tier for everything else — no subprocesses and no system dependencies by
default, so it runs the same on any machine and in CI.

## Two coverage tiers

- **Tier 1 — native in-process backends.** Highest fidelity, compiled straight into the
  binary: `ruff` (Python), `oxc` (JS/TS/JSX), `taplo` (TOML), `rumdl` (Markdown), plus
  `sqruff`, `malva`, `markup_fmt`, `graphql`, `nixfmt`, `typos`, and YAML. Registered per
  language in the engine registry.
- **Tier 2 — tree-sitter generic tier.** The catch-all for the long tail (shell, Go, Java,
  Kotlin, Ruby, PHP, C/C++, Dockerfile, protobuf, and 300+ grammars). CST-driven structural
  reindent and whitespace normalization — best-effort, pure Rust, grammars fetched on demand.
- **Native-toolchain backends (scoped exception).** A language's canonical first-party CLI
  (`rustfmt`, `gofmt` default-on when present; `zig fmt` and lint tools opt-in) is invoked
  per file over stdin/stdout when installed. When absent, the language falls through to
  tier 2, so the zero-dependency guarantee always holds.

## When to use it

Reach for poly as the single lint/format gate for any repo: local dev, git hooks, and CI.
It replaces invoking ruff / eslint / rustfmt / prettier individually — one binary, one
`poly.toml`, one report. A language with no native backend still gets tier-2 formatting, so
poly covers the whole tree rather than only the languages you wired up by hand.
