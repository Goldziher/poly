# 0044 — skif as an Opt-In Project Check in the web Component

- Status: Accepted
- Date: 2026-11-07
- Amends: ADR 0039
- Related: ADR 0040, ADR 0041

## Context

ADR 0039 named skif as a user of the project-check capability, and ADR 0041 showed the shape: an
engine in the `web` component declares its inputs, the host loads them and caches the result on their
digests. skif lints JavaScript and TypeScript workspaces (manifests, version policy, catalogs,
unused and missing dependencies, dead exports) across npm, pnpm, yarn and bun, and it discovers a
workspace by reading the file system: manifests, lock files, `tsconfig`, `.gitignore` and sources,
and, for installed binaries, `node_modules`. The decisions are how those reads reach the cache key,
whether the engine runs unasked, how `--fix` reaches a project check, and how skif's oxc relates to
poly's.

## Decision

- **Opt-in, `[engines.skif] enabled = true`.** Finding a JavaScript workspace root is not consent to
  enforce dependency policy on it: the first run on an existing monorepo reports findings, and the
  other opt-in engines (adrlint, typos, `python-types`) avoid turning a green run red on upgrade the
  same way. Auto-activation was rejected for that reason; it can be a later default once the rule
  set's false-positive rate on real repositories is known.
- **The key is declared, not recorded.** The host computes the project-check key before the call, so
  a read set that is only known after skif runs cannot be part of it. The engine declares its inputs
  through `Engine::project_inputs`: the tree under `dir` (default `.`) walked recursively, serving
  JavaScript, TypeScript, Vue, Svelte, Astro, JSON, YAML and TOML (skif counts the single-file
  components as code; `.mdx` it does not read), every `.gitignore` and `.ignore` wherever the walk
  finds one, and the four lock files as side files, which the walk skips by rule. Manifests,
  `tsconfig`, `pnpm-workspace.yaml`, `skif.toml`, ignore files and sources are all in that set, so
  changing, adding or removing any of them changes the key.
- **The walk is the whole tree.** `ProjectDirs.whole_tree` makes the host honor only `.gitignore` and
  the always-pruned directories for this engine: not `[discovery] exclude`, not `--exclude`, not the
  split into nested `poly.toml` scopes. A verdict about an import graph is wrong when a member is left
  out for a reason that concerns linting, so an excluded `packages/legacy` still resolves the imports
  that reach it. The run's files then decide only which findings are reported: a finding on an excluded
  file is dropped. adrlint does not opt in and keeps its walk. `ProjectDirs.file_names` is the declared
  list of names handed over wherever they occur (`.gitignore`, `.ignore`) and only to the engine that
  declared them; both fields default on the wire, so the protocol version is unchanged.
- **Planned once, from the outermost config that enables it.** The candidates are the configs that own
  a file of the run and the configs above a path the run named, so `poly lint packages/a` is judged by
  the root config that enables skif, not by a fragment from `packages/a`'s own `poly.toml`. A nested
  scope's plan is dropped when an outer plan's resolved directories contain the nested root; an outer
  `dir = "frontend"` does not reach `backend/`, which a `backend/poly.toml` can then enable on its own.
  Findings are then attributed to the config governing their own file: a nested `enabled = false`
  silences its subtree, and a nested `[per-file-ignores]` or `[engines.skif.rules.<id>] level` applies to
  its files. The analysis options (`ignore`, `select`, `production`, `dir`, ...) are the outer plan's
  alone; a nested config that sets different ones gets a `nested-options-ignored` warning on its
  `poly.toml` rather than a silent override.
- **A path-scoped run still reads the tree.** Every run walks, reads and hashes every JavaScript,
  TypeScript, JSON, YAML and TOML file under `dir`, whatever paths it names, because the cache key is
  over that set and the verdict is about the whole graph; naming one file narrows the report, not the
  work. There is no size cap: a large generated JSON or YAML data file under `dir` is read, hashed and
  handed to skif like any other. A cap would need to be a counted omission (a skipped file that skif
  then reports unresolved or unused), which trades a cost for false findings, so it is left to
  `.gitignore`. Symlinks are not followed: a package reached only through a symlink (a linked
  workspace member) is absent from the graph, and what imports it is reported as unresolved.
- **skif runs hermetically on those files.** The `FileSystem` handed to `find_workspace_root`,
  `load_config`, `Linter::discover_with_fs` and `Linter::run_with` is an in-memory tree over the
  request's files that records every lookup; it never reads the disk. A result therefore cannot
  depend on a `node_modules` installed on one machine and not another (ADR 0039's "output must not
  depend on what is installed"), at the cost that skif treats packages as not installed. The record
  is the proof: a test asserts every file skif read is one the request carried, every directory it
  listed is implied by them, and the exact set of paths it looked for and did not find (the installed
  manifests, a gitignored import's target, optional config and lock files). `.gitignore` and `.ignore`
  are skif inputs: skif reads them to treat a relative import of a gitignored (generated) file as
  ignored rather than unresolved, and the host's walk applying them is not the same thing. The one
  thing it cannot be handed is above the project root: skif also looks for a `skif.toml` or
  `tsconfig.json` in the ancestors of the root, and those are not read.
- **`ProjectDirs` gains `recursive_default`.** ADR 0041's directory declaration defaulted to the
  top level unless a boolean option said otherwise; a workspace is a tree. The field is optional on
  the wire and reads `false` when absent, so the protocol version is unchanged.
- **The host applies project fixes.** Until now the project phase cleared the edits a check
  attached, because nothing applied them. Under `--fix` the host now commits them to the file the
  finding is in, with the per-file tier's rules: atomic groups, a generated file withheld unless
  `--fix-generated`, and a file that changed since it was checked left alone. Resolved findings
  leave the report and count as fixed. When a pass rewrote a file, each engine that had a finding in
  it runs once more over the rewritten tree and only that pass is reported for it, so spans are into
  the files as they now are, a finding the first pass exposed is seen, and a group the first pass
  deferred (it overlapped another edit) is tried again; there is no third pass. Every other engine's
  first-pass findings stand, since its input did not change, and the errors of both passes are
  reported, a repeated one once. A file rewritten in the last pass has its remaining spans dropped.
  Every engine's edits to one file are committed together. Without `--fix` the edits
  are still cleared. skif plans only
  safe fixes, one finding at a time, and attaches a fix only when it edits the finding's own file;
  unsafe fixes, fixes that need a choice and fixes that span files stay suggestions.
- **Rules that need installed packages are off unless the lock file covers them.** With no
  `node_modules`, skif falls back on a lock file. `package-lock.json`, `bun.lock` and a Berry
  `yarn.lock` record binary names and peers; `pnpm-lock.yaml` records that a package has a binary and
  skif assumes it is named like the package; a classic `yarn.lock` and no lock file record nothing.
  `unlisted-binaries` is off in every case: it needs the installed binaries. `unused-dependencies`
  and `unused-dev-dependencies` are off too unless the lock file in effect (skif's own precedence:
  pnpm, npm, bun, yarn) is one that records binaries, since otherwise a dependency run only through a
  binary named otherwise (`@acme/toolkit` running `kit`), or only the peer of another dependency, is
  reported as unused. A rule runs when the project names it: in `select`, in
  `[engines.skif.rules.<id>]`, or in its own `skif.toml` `[rules]`. The defaulting is visible: a run
  that left rules off reports one info-level `rules-defaulted-off` finding on the workspace's root
  `package.json`, naming them and how to turn them on. skif's table of well-known binaries (`ng` for
  `@angular/cli`) keeps those from firing even when asked for. The other degradation is silent: a
  `tsconfig` that extends into a package is read without the base, so `@types/*` are assumed all
  included and a restriction in the base is a missed finding, never a false one.
  `incompatible-peer-dependencies` and a binary supplied by an installed package are likewise only
  missed.
- **Config.** `[engines.skif]` maps onto skif's `ConfigOverrides` (`ignore`, `ignore_packages`,
  `ignore_dependencies`, `production`, `js`) and `select` onto its rule selection; `skif.toml` is
  found through the same file system. `threads` defaults to the machine's parallelism, because the
  request carries no job budget and reading and parsing every manifest and source of a monorepo on one
  thread is the bulk of a cold run; `threads = 1` spawns none. skif builds its pool for the call and
  drops it when the call returns.
- **One oxc.** skif's `skif-js-syntax` takes oxc `^0.157`, the version poly's git pin is at.
  `[patch.crates-io]` in `components/` redirects every oxc monorepo crate skif pulls to that git
  copy, so `cargo tree -d` gains no oxc duplicate. skif is depended on at an exact `=` version with
  `default-features = false` and `manifest`, `js-analysis` and `fix`; `report` stays off.
- **No per-file hooks.** The engine overrides none, so a warm run answers every hook from the index and
  never loads the component for a JSON, YAML or TOML file. The cost is the trait default for lint
  coverage, which reads a registered language as covered; every language skif serves also has its own
  lint engine, as Python has ruff beside `python-types`.
- **Presets** `skif` and `js-deps` are language aliases like `js`, and select the `web` component.

## Consequences

`poly lint` can judge a JS/TS monorepo's dependency graph without a skif install, and a cached
verdict is invalidated by exactly the files skif can see. Dependency findings that depend on what is
installed (a binary provided by an installed package) are not reported. The cost is every JSON,
YAML, TOML, JS/TS and component file under `dir` being read and hashed on every run to compute the
key, including ones skif ignores, before the cache can answer: a warm run over a large monorepo is
bounded by reading its tree, not by skif. The walk skips `node_modules` and the other pruned
directories; a project that keeps a huge tree out of them only by `[discovery] exclude` pays for it.
`--fix` for project checks is now a host feature other project engines can use by attaching edits.
