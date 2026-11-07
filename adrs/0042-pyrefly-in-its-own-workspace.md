# 0042 — pyrefly Lives in Its Own Cargo Workspace

- Status: Accepted
- Date: 2026-11-06
- Amends: ADR 0040
- Related: ADR 0038, ADR 0039

## Context

ADR 0038 makes the Python type checker selectable per project: `[engines.python-types] backend = "ty"`
or `"pyrefly"`. ty is a crates.io dependency in the `python` component. pyrefly is not published: the
crates.io `pyrefly` 0.0.1 is an unrelated crate and its `pyrefly_*` workspace crates do not exist there,
so it is a git dependency, which ADR 0040 confines to the `components/` workspace.

That workspace cannot hold it. `pyrefly`'s library crate depends on `tikv-jemallocator 0.7`
unconditionally (the allocator is chosen in the `pyrefly` binary only, but the dependency is on the
library, behind no feature), while `rumdl`, which the `web` component links, depends on
`tikv-jemallocator 0.6`. Both pull a `tikv-jemalloc-sys` with `links = "jemalloc"`, and Cargo allows one
such crate per lockfile. Disabling pyrefly's default features drops its bundled third-party stubs but
not the allocator dependency.

## Decision

pyrefly lives in `components-pyrefly/`, a cargo workspace with its own `Cargo.lock` and `deny.toml`.

- The bridge crate `poly-engines-pyrefly` and the alef-generated `poly-engines-pyrefly-component` are the
  only members. The workspace depends on poly's root crates by path, like `components/`.
- pyrefly is pinned to the commit of a release tag (1.4.1), one `rev` for every pyrefly crate. Its
  `allow-git` entry is the only one in that workspace's `deny.toml`. Default features are off: the
  bundled third-party stubs are about 36 MB and ty does not ship them either.
- The `pyrefly` component is opt-in. alef builds it through `source_manifest` and `producer_dir` in
  `alef.toml`, exactly as for the other git-pinned families, so the release matrix, the content-addressed
  plan and the signed lock treat it like any other component.
- The engine id is `pyrefly`, not `python-types`: the component index gives every engine one component.
  The two engines share the `python-types` table through a declared selection (`Engine::selection`, carried
  on the wire as `WireSelection {table, key, value, default}`): ty declares `backend = "ty"` as the default,
  pyrefly declares `backend = "pyrefly"`. The planner runs the engine whose value the key holds and the
  component planner fetches only that engine's component; a value nobody declares plans the default, which
  reports it as unknown. An unset key plans neither, so type checking stays opt-in.
- The check forces `skip_interpreter_query` and hands pyrefly the `site-packages` paths the host
  resolved, so no process is started. It runs through `State` and `Transaction` over an in-memory overlay
  of the files the host passed.
- The root `Cargo.lock` still has no git source. `mise run check:components-lock`, the version bump and CI
  cover both component workspaces. The crates.io watch lists the pyrefly crates, so the pin is retired
  when a real release appears.

## Consequences

- A third lockfile and deny config. pyrefly pulls its own `ruff_*` 0.0.14 crates (nine of them), which duplicate the
  0.0.21 ones in the root workspace. They never meet in one lockfile or binary, and `cargo tree -d` inside
  `components-pyrefly` shows no duplicate ruff crate, only the common transitive ones (`syn`, `hashbrown`, `rand`, ...).
- The pyrefly component is not part of the `poly-bundled` binary, which cannot link it.
- ty and pyrefly report different rule codes; a config that switches backend has to update its
  `[per_file_ignores]` and `poly: allow[...]` directives.
- Files that import one another are read from disk by pyrefly even when the host passed other contents
  for them; the checked file itself uses the passed contents.

## Cross-file limitation

pyrefly in poly sees only the files the host handed it as in-memory contents, plus whatever it reads from
disk to resolve their imports (first-party roots, `extra_paths`, the host-resolved `site-packages`). It
does not discover the project's other files, so a module that nothing handed over imports is never checked
and a finding that depends on a not-yet-saved edit to an imported module is computed against the saved
copy. In practice a whole-project run is exact, and a run over a path subset reports findings only in those
files, resolved against the rest of the tree as it is on disk. ty behaves the same way.

## Measurements

aarch64-apple-darwin, release profile with thin LTO and stripped symbols (the release workflow's flags).

| | pyrefly component | python component (ruff and ty) |
|---|---|---|
| cdylib | 15.4 MB (14.7 MiB) | 31.5 MB (30.0 MiB) |
| `.tar.gz` | 7.5 MB (7.1 MiB) | 12.6 MB (12.1 MiB) |

A cold `poly lint --no-cache --only <engine>` over a small project (eight modules, two type errors each, both
backends reporting the same sixteen findings), with the core binary loading the component from a file:// lock:
first run 1.03 s for pyrefly and 1.24 s for ty, both including installing and loading the component; later runs with
the result cache still off 0.08 to 0.11 s for pyrefly and 0.10 to 0.17 s for ty. The first run is dominated by the
install, not the check, so the figures say the backends cost about the same on a small project, not which one
scales better.

## Alternatives considered

- **Make rumdl's allocator optional or bump it to 0.7.** Rejected: it is upstream work outside poly's
  control, and it would still leave pyrefly in a workspace with the oxc and biome pins.
- **Drop pyrefly.** Rejected: ADR 0038 and the epic promise both backends.
- **Run the pyrefly binary as a subprocess.** Rejected: it breaks the no-subprocess rule (ADR 0002) and
  would need an interpreter query of its own.
- **Keep it in `components/` with a patched `rumdl`.** Rejected: a fork of rumdl to maintain, ADR 0003
  forbids vendoring.
