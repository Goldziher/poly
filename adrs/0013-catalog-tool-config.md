# 0013 — Catalog-Driven Tool Config: Retire .pre-commit-config.yaml

- Status: Accepted, implementation pending
- Date: 2026-06-28

## Context

The native hook runner (ADR 0012) replaces the prek bridge and eliminates
`.pre-commit-config.yaml` for builtin and inline hooks. However, many repositories
configure third-party tools (linters, formatters, static analyzers) via pre-commit hooks.
These tools have their own install/invoke semantics (binary names, arguments, stdin
conventions, supported languages).

Currently, users must maintain both `poly.toml` (for lint/format) and
`.pre-commit-config.yaml` (for additional tools). The north-star is to unify this: every
tool is configurable from `poly.toml`, and `.pre-commit-config.yaml` becomes unnecessary.

## Decision

- **Vendor the tool-catalog data from mdsf** (MIT) — the `tools/*/plugin.json` mappings
  of tool → binary → arguments → stdin convention → languages. Build this into `poly` as a
  registry.
- **Every tool becomes configurable from `poly.toml`.** The `[tools]` section defines
  which tools to enable, their arguments, which git stage they run in, file patterns they
  match. Example: `[tools.rust.clippy] enabled = true, args = ["--deny", "warnings"],
  stage = "pre-push"`.
- **Classify tools by their execution model:**
  - **Single-file/stdin tools** (e.g. `prettier`, `gofmt`, `rustfmt`) → become
    native-toolchain backends (ADR 0014) when enabled.
  - **Project-wide tools** (e.g. `cargo clippy`, `eslint`, `golangci-lint`) → become
    hook builtins, not per-file engines (they are not amenable to the rayon per-file
    unit).
- **Capability-probe and declare.** On startup, probe for each enabled tool's presence
  and version; declare its capability (lint / format / both) only when found and enabled.
  Off by default; presence is not required.
- **Catalog evolution is manual.** We pin the catalog snapshot from mdsf at a specific
  commit; updating it is a deliberate change in `poly.toml` or a new ADR.

## Consequences

Positive:

- `.pre-commit-config.yaml` is fully retired. One `poly.toml` drives everything:
  lint/format engines, git-hooks, commit linting, and third-party tools.
- Users do not clone or download tool repos; tools are discovered on `PATH` (for
  native-toolchain backends) or come from `poly`'s vendored list. Setup is simpler.
- Tool configuration is centralized and uniform; tool arguments, git stages, and file
  patterns are all in one place.
- Capability-probing means users can commit `poly.toml` with all tools declared, and
  missing tools gracefully degrade rather than break the build.

Negative / risks:

- The tool catalog is large (100+ tools) and requires maintenance. When new tools emerge
  or upstream tools change their binary names, the catalog must be updated. This is a
  manual, out-of-band process.
- Single-file/stdin tools are classified correctly only by empirical testing; a tool
  that claims stdin support but doesn't actually use it will cause confusion.
- Project-wide tools (like clippy) cannot leverage the content-hash cache effectively
  because they see the whole workspace, not individual files. They may be slower than
  expected or require explicit cache-disable.

## Alternatives considered

- **Ship with no tool catalog; let users define tools by hand:** rejected — defeats the
  purpose of unification. The catalog is what makes third-party-tool integration
  frictionless.
- **Depend on the mdsf crate directly:** rejected — we need only the catalog data, not
  the mdsf binary or its full CLI surface. Vendoring the data decouples us from mdsf's
  versioning and dependencies.
- **Auto-clone repos from `.pre-commit-config.yaml` entries:** rejected — we're trying to
  eliminate the need for `.pre-commit-config.yaml`, not enhance it.
- **Per-tool opt-in via a plugin system:** rejected — a Rust plugin system adds complexity
  and a runtime. The catalog approach is simpler for v1.
