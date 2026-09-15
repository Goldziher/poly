---
priority: medium
description: "What poly is — the native, tree-sitter, native-toolchain, quality, and built-in-rule-pack tiers, language coverage, and when to reach for it"
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:1caf5d0228e11f40e7960822a9f21f3505a2e1be5b0ebdda990eb92b91da3762
Source-Hash: blake3:b0fbdd459a2f14c765bb72bb51e4541aec552d2bd3b84d6497457dbd68012c24
Schema-Version: v1
-->

# poly Overview

poly is a single pure-Rust binary that lints and formats a whole repository across
languages. It wraps best-in-class tools as in-process crate backends and falls back to a
tree-sitter generic tier for everything else, so it runs the same on any machine and in CI.
No system tool is ever *required*; the one scoped exception is the native-toolchain tier
below, which uses a language's canonical CLI when it happens to be installed.

## Per-language coverage tiers

- **Tier 1 — native in-process backends.** Highest fidelity, compiled straight into the
  binary and registered per language in `registry.rs`: `ruff` (Python), `oxc`
  (JS/TS/JSX/TSX/JSON/JSONC), `taplo` (TOML), `rumdl` (Markdown/MDX), `sqruff` (SQL), YAML,
  `malva` (CSS/SCSS/LESS) + `biome` (CSS/SCSS), `markup_fmt` (HTML/Vue/Svelte/Astro/Angular/Jinja/
  Vento/Mustache/XML), `mago` (PHP), `rubyfmt` (Ruby), `graphql` + `biome` (GraphQL),
  `nixfmt` (Nix), `hcl`, Dockerfile, dotenv, and INI.
- **Tier 2 — tree-sitter generic tier.** The catch-all for the long tail (Elixir, C/C++,
  C#, protobuf, and the rest of 300+ grammars). CST-driven structural reindent for
  brace-family grammars, whitespace normalization otherwise, and a `LEAVE_UNTOUCHED` list
  for grammars where whitespace is significant. Best-effort, pure Rust, grammars loaded on
  demand — never gofmt/rustfmt parity, by design.
- **Native-toolchain tier (scoped exception).** A language's canonical first-party CLI run
  per file over stdin/stdout when installed: `rustfmt`, `gofmt` and `shellcheck` are **on by
  default when found on PATH**; `zig fmt`, `shfmt`, `google-java-format`, `ktfmt`, `styler`
  (R), `swift-format`, `dart format`, and `gleam format` are opt-in via
  `[fmt.<lang>.<tool>] enabled = true` / `[lint.<lang>.<tool>] enabled = true`. When the
  tool is absent, formatting falls through to tier 2 with an info-level notice, so the
  zero-dependency guarantee always holds.

## Cross-cutting engines (every language)

Four engines are appended to every language regardless of tier:

- **`typos`** — spelling in identifiers, comments, and strings.
- **`quality`** — structural code-quality metrics off the tree-sitter parse: file/function/
  type length, nesting depth, cyclomatic complexity, parameter count, and `lazy-ignore`.
  Warning severity, on by default. The structural rules need a verified node-kind table and
  run for Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C#, and Ruby;
  `magic-number` and `law-of-demeter` are opt-in. Configure under `[lint.quality]`.
- **`astgrep`** — ast-grep pattern rules. poly embeds a **built-in rule pack** of 26 rules
  across 9 languages (C#, Elixir, Go, Java, Kotlin, Python, Ruby, Rust, Swift), on by
  default; 13 of them ship `severity: off`. User rules under `[rules] dirs` (default
  `.poly/rules`) layer on top and replace a pack rule sharing its `id`. Disable the pack
  wholesale with `[rules] builtin = false`.
- **`uncomment`** — opt-in comment removal, off unless `[lint.uncomment] enabled = true`.

## When to use it

Reach for poly as the single lint/format gate for any repo: local dev, git hooks, and CI.
It replaces invoking ruff / oxlint / rustfmt / prettier individually — one binary, one
`poly.toml`, one report. A language with no native backend still gets tier-2 formatting
plus the cross-cutting engines, so poly covers the whole tree rather than only the
languages you wired up by hand.
