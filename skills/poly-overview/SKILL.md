---
description: What poly is — the native, tree-sitter, native-toolchain, quality, and built-in-rule-pack tiers, language coverage, and when to reach for it
name: poly-overview
license: MIT
metadata:
    author: Goldziher
    repository: https://github.com/Goldziher/polylint
---

<!--
AI-RULEZ :: GENERATED FILE — DO NOT EDIT
Content-Hash: blake3:06c6c5ee1b63cdeca58dd7321947ba41b8b9668a10fc30e8e5277acaa67e743c
Source-Hash: blake3:67d1d4dd812fd5dac9dbb204c4875dcd559d020f6c51e00f52c2816a9c24854f
Schema-Version: v1
-->

# poly Overview

polylint (the command is `poly`, the config `poly.toml`) is one linter and formatter for every
language in a repository. Its engines (ruff, oxc, biome, mago, sqruff, …) ship as signed,
checksum-verified **components** that a core binary downloads the first time a run needs them;
it also runs a language's canonical host CLI when installed (`rustfmt`, `gofmt`, `shellcheck`),
ecosystem tools as opt-in **recipes**, and a tree-sitter tier for the long tail.

A first run on a core build may download components and tree-sitter grammars. A needed component
that cannot be installed is exit `2` (or, with `[components] on_missing = "skip"`, a counted skip
listed in `components.missing`), never a silent downgrade. Only the Linux musl release binary is
bundled with every engine linked in.

## Per-language coverage tiers

- **Tier 1 — engine components.** Highest fidelity, registered per language: linked
  into a bundled build (the Linux musl release binary) or loaded from signed components that a core
  build (every other release binary) downloads on first use (`poly components list`). Listed by the engine id you configure them
  under (`[engines.<id>]`): `ruff` (Python), `oxc`
  (JS/TS/JSX/TSX/JSON/JSONC), `taplo` (TOML), `rumdl` (Markdown/MDX), `sqruff` (SQL), `yaml`,
  `malva` (CSS/SCSS/Less) + `biome` (CSS/SCSS), `markup_fmt` (HTML/Vue/Svelte/Astro/Angular/
  Jinja/Vento/Mustache/XML), `mago` (PHP), `rubyfmt` (Ruby), `graphql` + `biome` (GraphQL),
  `alejandra` (Nix), `hcl`, `dockerfile`, `dotenv`, and `ini`. YAML, TOML, Markdown and CSS
  format with the same crates oxfmt uses, and `oxc` honours an existing `.oxfmtrc.json[c]` /
  `.oxlintrc.json[c]`; `hcl` output matches `terraform fmt`.
- **Tier 2 — tree-sitter generic tier.** The catch-all for the long tail (Elixir, C/C++,
  C#, protobuf, and the rest of 300+ grammars). CST-driven structural reindent for
  brace-family grammars, whitespace normalization otherwise, and a `LEAVE_UNTOUCHED` list
  for grammars where whitespace is significant. Best-effort, grammars fetched on demand —
  never gofmt/rustfmt parity, by design.
- **Host tools.** A language's canonical CLI run as a subprocess when installed: `rustfmt`,
  `gofmt` and `shellcheck` are **on by default when found on PATH**; `zigfmt`,
  `google-java-format`, `ktfmt`, `styler` (R), `swift-format`, `dartfmt`, and `gleamfmt` are
  opt-in via `[engines.<id>] enabled = true`. When the tool is absent, formatting falls through
  to tier 2 with an info-level notice.
- **Shell.** Formatted by the `sh` component (shfmt's Go library, run as a sidecar), not by a
  host `shfmt`. The host `shfmt` is used only by the bundled build or with
  `[components] disable = ["sh"]`.
- **Catalog and recipe tools (opt-in).** Any tool in poly's vendored catalog —
  `golangci-lint`, `actionlint`, `hadolint`, `helm`, `tflint`, `buf`, `buildifier`, … — runs
  once you set `[tools.<name>] enabled = true`. Nothing is downloaded unless you also pin
  `version` and set `install = "auto"`, which fetches a checksum-verified release into poly's
  tool cache; otherwise a missing binary logs a warning and the tool is skipped. JVM, Node and
  Python tools (`checkstyle`, `npm-groovy-lint`, `pyproject-fmt`) use the runtime on the host;
  poly never bundles a runtime, so do not enable one on a machine without it.

## Cross-cutting engines (every language)

Four engines apply to every language regardless of tier (`typos` only when enabled):

- **`typos`** — spelling in identifiers, comments, and strings. Opt-in: `[engines.typos] enabled = true`.
- **`quality`** — structural code-quality metrics off the tree-sitter parse: file/function/
  type length, nesting depth, cyclomatic complexity, parameter count, and `lazy-ignore`.
  Warning severity, on by default. The structural rules need a verified node-kind table and
  run for Python, Rust, Go, JavaScript, TypeScript, TSX, Java, Kotlin, C, C++, C#, and Ruby;
  `magic-number` and `law-of-demeter` are opt-in. `uncollected-test-file` flags a file named
  like a test that the language's test runner will not collect (`parser.test.py` under
  pytest) — on for Python, Go and Ruby, off for JavaScript/TypeScript. Configure under
  `[engines.quality]` / `[engines.quality.lang.<lang>]`.
- **`astgrep`** — ast-grep pattern rules. poly embeds a **built-in rule pack** of 26 rules
  across 9 languages (C#, Elixir, Go, Java, Kotlin, Python, Ruby, Rust, Swift), on by
  default; 13 of them ship `severity: off`. User rules under `[engines.astgrep] rule_dirs`
  (default `.poly/rules`) layer on top and replace a pack rule sharing its `id`. Disable the
  pack wholesale with `builtin_pack = false` in the same table.
- **`uncomment`** — opt-in comment removal, off unless `[engines.uncomment] enabled = true`.

## When to use it

Reach for poly as the single lint/format gate for any repo: local dev, git hooks, and CI.
It replaces invoking ruff / oxlint / rustfmt / prettier individually — one binary, one
`poly.toml`, one report. A language with no native backend still gets tier-2 formatting
plus the cross-cutting engines, so poly covers the whole tree rather than only the
languages you wired up by hand.
