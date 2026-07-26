# 0020 — Shared Configuration via `extends` (Local and Pinned Remote Bases)

- Status: Accepted
- Date: 2026-07-26

## Context

ADR 0018 gave a monorepo hierarchical config: nested `poly.toml` files cascade from the
workspace root down to each sub-project's directory. That solves in-repo nesting, but it
stops at the repository boundary by design (a `.git` directory, or `[workspace] root =
true`, bounds the cascade). It does not solve the orthogonal case of an organization that
wants a **shared baseline across repositories** — one `line_length`, one `[lint.*]`
rule set, one `[hooks.builtin]` policy — applied consistently to a dozen independent repos,
each with its own `.git`. Today that baseline can only be copy-pasted into every repo's
`poly.toml`, which drifts the moment one repo's copy is edited and not the others.

`[[hooks.sources]]` (ADR 0012/0013) already solves this exact problem for hook catalogs:
a repo declares a pinned local or Git source and inherits hook definitions from it, with
a lock file (`poly-hooks.lock`) pinning symbolic refs to a resolved OID. Config sharing is
the same shape applied to the rest of `poly.toml` — `[discovery]`, `[lint.*]`, `[fmt.*]`,
`[tools.*]`, `[per-file-ignores]`, `[hooks.*]`, `[defaults]`, and so on — so it should reuse
the same vocabulary and the same trust model rather than invent a second one.

## Decision

- **A top-level `extends` list in `poly.toml`.** Each entry is either a bare string (a local
  path shorthand) or a table mirroring `[[hooks.sources]]`: `path` (local) XOR `git` (a
  repository URL), `revision` (required and nonempty for `git`), `file` (path to the config
  file within the repo/directory, default `poly.toml`), and an optional `id`. Later entries
  take precedence over earlier ones:

  ```toml
  extends = [
    { git = "https://github.com/acme/poly-baseline", revision = "<40-hex-oid>", file = "poly.toml" },
    "./poly.overrides.toml",   # later entry = higher precedence
  ]
  ```

- **Layering order (lowest to highest precedence):** tool defaults → `[defaults]` →
  `extends` bases in listed order (each fully resolved, including its own transitive
  `extends`) → this `poly.toml` → this `poly.local.toml` → the ADR-0018 ancestor-directory
  cascade (the root config's fully-resolved table becomes the base; each nearer directory
  merges on top). `extends` resolution therefore happens once per config, before the
  directory cascade sees it — a nested `poly.toml` inherits its own `extends` bases, not
  its ancestor's.
- **Deep-merge, not concatenation.** Merging reuses the existing raw-table `merge_tables`
  (ADR 0006/0018): scalars and arrays replace, tables merge key-by-key. `extends` lets a
  config inherit *any* section — it is not restricted to a subset of the schema.
- **Transitive chains, bounded and cycle-checked.** A base may itself declare `extends`
  (org → team → repo). Chains are resolved depth-first in listed order; a cycle is a hard
  error, and resolution is capped at depth 32 to keep pathological chains from hanging a
  run.
- **Relative paths in an inherited section stay anchored to the consumer.** A base's
  `[rules] dirs` (or any other path-bearing key) resolves relative to the directory of the
  config that ultimately declares `extends` — not the base's own directory. A shared
  baseline sets policy; it does not reach into the consumer's filesystem layout.
- **`extends` is forbidden in `poly.local.toml`.** The machine-local override layer must
  never itself pull in a remote base — that would let an uncommitted, unreviewed file
  change what code runs on a machine. `poly.local.toml` stays what ADR 0006 defined it as:
  a local override *on top of* an already-resolved config.
- **`--config <path>` still applies that file's `extends`.** The explicit-config escape
  hatch (ADR 0006/0018) picks one file; that file's own `extends` chain still resolves
  normally.
- **`[workspace] root = true` bounds only the ADR-0018 directory cascade**, not `extends`.
  The two mechanisms are independent: one walks a repo's own directory tree, the other
  crosses repository boundaries entirely.
- **Reproducibility mirrors the `[[hooks.sources]]` model.** A `git` base pinned to a full
  40- or 64-hex OID self-pins — nothing to lock, fetched once, cached, offline after. A
  symbolic ref (branch or tag) requires a lock: a new `poly config update` subcommand
  resolves ref → OID and writes `poly-config.lock` (a config-specific lock, separate from
  `poly-hooks.lock`) to the repo root. Loading a symbolic-ref base with no lock entry is a
  hard error directing the user to run `poly config update` first — normal `poly lint`/`poly
  fmt` runs never resolve a floating ref themselves.
- **New CLI surface:** `poly config update` (resolve symbolic refs, write/refresh
  `poly-config.lock`) and `poly config resolve` / `poly config show` (print the fully
  merged, effective config — the same purpose `ruff check --show-settings` or `eslint
  --print-config` serve, needed here because a config can now be built from several files
  across two or more repositories).
- **`poly-config` stays network-free.** As with `[[hooks.sources]]`, the crate that parses
  and merges config never performs I/O beyond the local filesystem; the CLI injects a
  resolver that fetches and caches remote git bases (reusing the sanctioned git-subprocess
  path from ADR 0012/0013) and hands `poly-config` only already-materialized local paths.
- **Security: extending a remote config is a trust decision, not a convenience toggle.**
  A base can declare `[hooks]`/`[tools]` that run arbitrary commands, so pulling one in
  means trusting that repository to execute code on your machine — the same trust boundary
  ADR 0012 already draws for `[[hooks.sources]]`. Mitigations, none of them new inventions:
  - **Mandatory pinning on the load path.** No floating ref is ever resolved by a normal
    run; a symbolic ref is pinned only by an explicit, human-run `poly config update`,
    which prints the resolved OID and summarizes the `[hooks]`/`[tools]` entries the base
    introduces so the pin is a reviewable action, not a silent one.
  - **Origin-URL verification** of the git mirror before fetch, defeating `insteadOf`
    rewrite attacks — the same check `[[hooks.sources]]` already performs.
  - **Read-only, tamper-checked checkouts**, content-addressed by the resolved OID, shared
    with the `[[hooks.sources]]` cache discipline.
  - **Hooks a base introduces still only run when the consumer's `poly.local.toml`
    declares `hook_preferences.channels`** — inheriting `[hooks]` via `extends` does not
    bypass the existing per-machine opt-in ADR 0012 requires.
  - **`extends` banned in `poly.local.toml`** (above) closes the one path that would let an
    unreviewed, ungated file introduce a remote base.

## Consequences

Positive:

- An organization defines one baseline `poly.toml` (or a small org → team → repo chain) and
  every repository extends it, instead of copy-pasting sections that drift.
- The mental model is uniform: `extends` reuses the exact `path`/`git`/`revision` vocabulary,
  cache, and lock discipline `[[hooks.sources]]` already established, so there is nothing new
  to learn for a team that already shares hooks this way.
- `poly config resolve`/`show` gives a single place to see the fully merged config a repo
  actually runs with, which matters more once a config can span several files and repos.

Negative / risks:

- Resolution is no longer "one repo, one file, maybe a local override": debugging which
  setting came from where now requires knowing the `extends` chain in addition to the
  ADR-0018 directory cascade. `poly config resolve` is the direct mitigation.
- A remote base is a supply-chain surface: a compromised or malicious baseline repo can
  inject hooks or tool config. The mitigations above (mandatory pinning, origin
  verification, tamper-checked checkouts, the existing hook opt-in gate) reduce but do not
  eliminate this risk — teams that extend a remote base are choosing to trust it, same as
  adding any dependency.
- A second lock file (`poly-config.lock`, alongside `poly-hooks.lock`) is one more file a
  repo commits and one more thing `poly config update` must keep current; see Alternatives
  for why it is not folded into a single lock.
- Transitive chains add a resolution cost (bounded by depth 32) and a class of bug (cycles)
  that a flat, single-file config never had.

## Known limitations (v1)

- **Transitive bases are trusted transitively.** `poly config update` locks and prints the
  `[hooks]`/`[tools]` of the top-level config's *direct* git bases, then also reports the
  `[hooks]`/`[tools]` of the fully-merged effective config as a whole. But a base you pin may
  itself `extends` further bases — and a nested base pinned to a full object ID needs no lock
  entry of its own, so it is fetched and merged without a separate review step. Pinning a
  remote base therefore trusts that base's *entire* transitive chain to run code on your
  machine. `update` prints a `note:` to this effect. A future version may lock the full
  transitive set.
- **Symbolic ref vs object ID is length-based.** A `revision` of exactly 40 or 64 hex
  characters is treated as a self-pinning object ID (no lock required). A branch or tag whose
  name happens to be all-hex of that exact length is indistinguishable from an OID and bypasses
  the lock step — a pathological but real edge. Name refs normally.
- **The MCP server resolves local `extends` only.** `poly mcp` uses the network-free resolver
  (the git resolver lives in `poly-cli`, which the server cannot depend on without a cycle), so
  a repo extending a remote git base must be served via the `poly` CLI, not the MCP server.

## Alternatives considered

- **Raw HTTPS URL transport (fetch a config file directly over HTTPS, no git):** deferred.
  It would avoid a git clone for the simple case, but it forfeits everything a git checkout
  gives for free — content-addressed pinning by commit, the existing origin-URL/mirror
  verification, and one fetch/cache path shared with `[[hooks.sources]]` instead of two.
  Revisit only if the git dependency proves too heavy for a common case `[[hooks.sources]]`
  hasn't already justified.
- **Unified `poly.lock` covering both hooks and config, instead of separate
  `poly-hooks.lock` / `poly-config.lock`:** deferred. `[[hooks.sources]]` already ships
  `poly-hooks.lock` with its own key shape (source id → resolved OID) and its own update
  command; merging the two lock files now would mean touching the stable hooks-locking code
  to accommodate a new consumer for marginal benefit. A single `poly.lock` is worth
  revisiting once both locking paths have settled, but splitting them today keeps this
  change additive and low-risk to the existing hooks feature.
- **A single `extend = "path"` string (ruff-style), instead of an `extends` list:**
  rejected. ADR 0018 rejected ruff's nearest-wins-plus-`extend` model *for the in-repo
  directory cascade*, where implicit inheritance is the better default. That rejection does
  not apply here: `extends` is solving a different problem (cross-repo baselines), where the
  ability to compose more than one base — an org baseline plus a team override, for
  instance — is the point. A single string would force everything into one base file or
  back into copy-paste, the exact problem `extends` exists to remove.
