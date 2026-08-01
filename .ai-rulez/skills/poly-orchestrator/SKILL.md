---
priority: medium
description: "Use poly as the single lint/format gate instead of invoking ruff/eslint/rustfmt directly — one poly.toml, poly hooks install, poly migrate, CI via the Goldziher/poly Action"
---

# poly as the Orchestrator

Treat poly as the **single** lint and format gate for the repository. Do not invoke ruff,
eslint, prettier, rustfmt, or the rest directly — poly wraps them (or covers them via the
tree-sitter tier) behind one binary, one config, and one report.

## Adopt it

- **One `poly.toml`.** Configure every language and rule in a single per-repo file
  (`poly.local.toml` for local overrides, `extends` to share a base). Layering is
  tool default → poly's opinionated overrides → your `poly.toml`.
- **`poly migrate`** — bootstrap `poly.toml` from an existing tool sprawl
  (ruff/eslint/prettier/pre-commit configs) instead of hand-writing it.
- **`poly hooks install`** — wire the git-hook shims so `poly` runs the configured
  pre-commit stage on every commit, replacing a `.pre-commit-config.yaml`.

## CI

Run poly in CI via the published GitHub Action:

```yaml
- uses: Goldziher/poly@v0
```

Then gate on `poly fmt --check .` and `poly lint .`. Because poly is one pinned binary with
no system dependencies, local, hook, and CI runs all produce the same result — pin the
toolchain only if you enable a native-toolchain backend (`rustfmt`/`gofmt`), whose output
depends on the host tool version.
