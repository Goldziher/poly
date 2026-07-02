# 0018 — Hierarchical Configuration Resolution for Monorepos

- Status: Accepted
- Date: 2026-07-02

## Context

ADR 0006 established one canonical `poly.toml`, discovered by walking upward from the
run root to the nearest ancestor and applied globally to every file in the run. That is
correct for a single-project repository, but it breaks in a **monorepo / polyrepo**: a
tree with a Rust core at the root plus, say, a `frontend/` app and a `docs-site/` under
it cannot give each sub-project its own configuration. Running `poly lint .` from the
workspace root applies one config everywhere, so the only escape was to `[discovery]
exclude` a subtree and run a foreign tool there — exactly the fragmentation poly exists
to remove.

Every mature linter/formatter solves this with hierarchical config: ruff resolves the
nearest `pyproject.toml`/`ruff.toml`, eslint's flat config cascades down the tree. poly
needs the same: run from the root, discover nested `poly.toml` files, and let each
govern its own subtree.

## Decision

- **Cascade / deep-merge resolution.** A file is governed by the deep-merge of its
  ancestor `poly.toml` chain — the workspace root as the base, the nearest config as the
  final override. A child declares only what differs and inherits `[defaults]`, the
  `[lint.*]`/`[fmt.*]` rule tables, `[per-file-ignores]`, etc. from its ancestors. This
  reuses the same table deep-merge already used for `poly.local.toml` (ADR 0006) and
  extends the layering philosophy of ADR 0007 across directories. It was chosen over the
  ruff-style *nearest-fully-governs with explicit `extend`* model because inheriting
  shared settings (one `line_length`, one ignore set) is the ergonomic default a monorepo
  wants; re-declaring them in every sub-project is the friction we are removing.
- **`[workspace] root = true` boundary.** The upward cascade stops at (and includes) the
  first config marked `[workspace] root = true`, so a project never inherits from a
  `poly.toml` above its own root (e.g. one in `$HOME`). A directory containing `.git` is
  an implicit boundary even without the marker, so the common single-repo case needs no
  annotation.
- **Excludes are unioned tree-wide, rooted per config directory.** Each in-tree config's
  `[discovery] exclude` globs prune only that config's own subtree: a nested glob is
  rewritten relative to its config directory (`frontend/`'s `dist/**` → `frontend/dist/**`)
  before being fed to the single file walk, preserving the `dir/**` → prune-the-directory
  optimization. Discovery excludes are therefore **additive** across the tree, while
  lint/format rules and defaults **cascade** per file. This split is deliberate: excludes
  are a whole-file discovery concern (a parent's exclude already prunes a child's subtree),
  whereas rules are a per-file semantic concern.
- **Planning keyed by `(config, language)`.** Engine plans (and their cache-key args) are
  built once per distinct `(config_id, language)` pair rather than once per language. A
  monorepo has a handful of distinct configs, so the per-file hot loop stays cheap; a
  single-config repo collapses to one plan per language — identical to before.
- **`[per-file-ignores]` globs are relative to their owning config's directory**, matching
  ruff/eslint pattern semantics: `"*.py"` in `frontend/poly.toml` matches `frontend/*.py`.
- **`--config <path>` bypasses nesting.** An explicit config pins that single config for
  every file and disables nested discovery — the escape hatch for "use exactly this".
- **Back-compatible.** A repo with one root `poly.toml` and no nested configs resolves
  every file to the root config, byte-for-byte the pre-hierarchical behavior. The ~20
  already-migrated repos are unaffected.

## Consequences

Positive:

- A monorepo runs `poly lint .` / `poly fmt .` from its root and each sub-project gets its
  own configuration — no more excluding a subtree and running a foreign tool there.
- Sub-project configs stay minimal: declare the diff, inherit the rest.
- One mental model shared with ruff/eslint; onboarding is "it cascades like ruff".

Negative / risks:

- Resolution is no longer "one file, applied everywhere": debugging which config governs a
  file requires knowing the nesting. Mitigated by the `.git`/`[workspace] root` boundary
  and by the additive-excludes / cascading-rules split being documented.
- A pre-walk scan for nested config files adds one directory traversal per run (pruned and
  gitignore-aware, filename-only), a small fixed cost on top of discovery.
- The excludes-union vs rules-cascade split is a subtlety users must learn; a nested config
  cannot *re-include* a path a parent excluded (union only prunes).

## Alternatives considered

- **Nearest-wins with explicit `extend` (ruff-style):** the closest `poly.toml` fully
  governs its subtree with no implicit inheritance; sharing is opt-in via
  `extend = "../poly.toml"`. Rejected as the default — predictable but forces every
  sub-project to re-declare or extend shared settings, which is the friction monorepos
  feel most. Cascade is the ergonomic default; `--config` remains for the "one exact
  config" case.
- **No nesting, exclude-and-delegate (status quo):** keep one config and exclude subtrees
  that need different tooling. Rejected — it reintroduces foreign config files and the
  fragmentation poly removes (this is exactly what forced `docs-site/biome.json` to linger).
- **Per-file config re-resolution (no dedup):** resolve a config for every file
  independently. Rejected on cost — memoizing by nearest-config directory and planning per
  `(config, language)` keeps the hot loop cheap while giving identical results.
