---
priority: high
---

# poly

poly is a single-binary, multi-language linter and formatter. It bundles engines (ruff, oxc, taplo, rumdl) and delegates to native tools (cargo fmt/clippy, golangci-lint, actionlint, shellcheck, shfmt) when present.

## Commands
- Lint: `poly lint .`
- Check formatting (dry-run): `poly fmt --check .`
- Apply formatting: `poly fmt --fix .`
- Apply lint autofixes: `poly lint --fix .`

## Configuration
Per-repo `poly.toml` (with `poly.local.toml` for local overrides). The result cache and hook
staged snapshot live in the per-user OS cache dir (`~/.cache/poly/<repo-key>` on Linux,
`~/Library/Caches/poly/…` on macOS, `%LOCALAPPDATA%\poly\…` on Windows) — not in-repo.
`POLY_CACHE_HOME` overrides the base; `[cache] dir` pins an explicit path.

## Severity
`poly lint` exits non-zero only on error-severity findings; warnings don't fail CI.

## CI
Validation runs via `uses: xberg-io/actions/.github/workflows/reusable-validate.yml@v1`.

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
