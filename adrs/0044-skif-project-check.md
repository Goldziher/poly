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
  split into nested `poly.toml` scopes (a whole-tree engine is planned once, from the outermost
  scope). A verdict about an import graph is wrong when a member is left out for a reason that
  concerns linting, so an excluded `packages/legacy` still resolves the imports that reach it. The run's
  files then decide only which findings are reported: a finding on an excluded file is dropped. adrlint
  does not opt in and keeps its walk. `ProjectDirs.file_names` is the declared list of names handed over
  wherever they occur (`.gitignore`, `.ignore`); both fields default on the wire, so the protocol
  version is unchanged.
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
  leave the report and count as fixed. When anything was fixed the check runs once more over the
  rewritten files and only that pass is reported, so spans are into the files as they now are, a
  finding the first pass exposed is seen, and a group the first pass deferred (it overlapped another
  edit) is tried again; a file rewritten in that last pass has its remaining spans dropped, and there is
  no third pass. Every engine's edits to one file are committed together. Without `--fix` the edits
  are still cleared. skif plans only
  safe fixes, one finding at a time, and attaches a fix only when it edits the finding's own file;
  unsafe fixes, fixes that need a choice and fixes that span files stay suggestions.
- **Rules that need installed packages are off by default.** With no `node_modules`, skif falls back
  on a lock file, which records peers for every manager but binary names only for npm, bun and yarn
  berry (pnpm records that a package has a binary, and skif assumes it is named like the package).
  `unlisted-binaries`, `unused-dev-dependencies` and `unused-dependencies` then report a dependency
  that is only run through a binary named otherwise (`@acme/toolkit` running `kit`), or only a peer
  of another dependency, as unused or its binary as unlisted; a pnpm lock removes the peer case and
  not the binary one. They run only when the project names them: in `select`, in
  `[engines.skif.rules.<id>]`, or in its own `skif.toml` `[rules]`. skif's table of well-known
  binaries (`ng` for `@angular/cli`) keeps those from firing even when asked for. The other
  degradation is silent: a `tsconfig` that extends into a package is read without the base, so
  `@types/*` are assumed all included and a restriction in the base is a missed finding, never a
  false one. `incompatible-peer-dependencies` and a binary supplied by an installed package are
  likewise only missed.
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
