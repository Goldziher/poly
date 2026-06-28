# 0002 — Pure-Rust, No-Subprocess, No-System-Dependency Constraint

- Status: Accepted
- Date: 2026-06-26
- Updated: 2026-06-28 (two scoped subprocess exceptions documented)

## Context

The mission (ADR 0001) is to eliminate system dependencies. There are two tempting
shortcuts that would quietly reintroduce them: shelling out to an installed tool, and
linking against a C/C++ library that must be present on the host. Either one breaks the
"nothing installed on the system" promise the moment a user's environment differs from
ours.

## Decision

Everything runs **in-process, in pure Rust**. The hard constraint:

- **No subprocess execution for lint/format engines.** Engines (defined in ADR 0004)
  never spawn `ruff`, `node`, `gofmt`, etc. Tools are consumed as Rust crate
  dependencies (see ADR 0003) and invoked via their library APIs.
- **Two scoped subprocess exceptions (see ADRs 0012 and 0014 for detail):**
  - **Native-toolchain backends (opt-in).** A language's canonical first-party CLI
    (e.g. `gofmt`, `rustfmt`, `zig fmt`) may be invoked as a subprocess when present
    and explicitly enabled in config. This is off by default; when absent, the language
    falls through to tier-2 generic formatting. See ADR 0014.
  - **Git-hook runner (`poly hooks`).** Running foreign git hooks inherently requires
    subprocess execution. The `poly-hooks` engine is the single in-process hook runner,
    and running user-supplied shell hooks via it is a documented feature. See ADR 0012.
- **No system dependency, ever (by default).** No required system shared libraries, no
  language runtimes, no tools assumed on `PATH` — as long as native-toolchain backends
  remain opt-in and disabled.
- The one sanctioned form of "downloading something" is the tree-sitter language pack
  fetching **precompiled grammars on demand** (ADR 0004) into a user cache. That is data,
  not a system dependency: it needs no toolchain, is version-pinned and hash-verified by
  the pack, and falls under our own cache discipline.

## Consequences

Positive:

- A single statically-oriented Rust binary runs identically on any machine; CI images
  drop every language runtime.
- The "zero-dependency proof" is testable: run polyfmt in a container with no
  python/node/go/etc. and every supported language must still format.
- Predictable performance and error handling — no process spawn overhead, no parsing of
  another tool's stdout/stderr.

Negative / risks:

- We can only consume tools that exist as (or can be reduced to) Rust libraries for
  engines. Tools that are CLI-only or hard-wired to C libraries cannot be wrapped;
  their languages fall to tier-2 generic formatting (ADR 0004) or native-toolchain
  backends (ADR 0014) instead.
- Some upstreams expose no stable library API, forcing vendoring (ADR 0003).
- We carry more compiled code in one binary; build times and binary size grow.
- The two subprocess exceptions (native-toolchain backends and git-hook runner) require
  strict discipline: they must be opt-in with graceful degradation, not the default, and
  must not compromise the zero-dependency guarantee for users who don't enable them.

## Alternatives considered

- **Subprocess/plugin model:** rejected — defeats the entire mission.
- **Optional native-library linkage (e.g. system libclang):** rejected — "optional system
  dep" still means "works on my machine, breaks in CI"; the constraint must be absolute to
  be trustworthy.
- **WASM-sandboxed plugins:** rejected for v1 — adds a runtime and toolchain of its own;
  pure in-process Rust is simpler and already sufficient.
