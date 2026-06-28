# 0014 — Toolchain Interop: Capability-Probed First-Party CLIs

- Status: Accepted
- Date: 2026-06-28

## Context

ADR 0002 commits to pure-Rust, in-process, no-subprocess execution. However, some
languages have canonical first-party formatters and linters for which no usable Rust
library exists: Go's `gofmt`, Rust's `rustfmt`, Zig's `zig fmt`, Rust's `cargo clippy`.
Reimplementing these tools is impractical and would never match the first-party behavior.

The solution is a scoped, opt-in exception: **native-toolchain backends** that invoke
these CLIs as subprocesses when present and explicitly enabled in config. This preserves
the zero-dependency guarantee for users who don't opt in, and gives power users the
option to use the canonical tool when available.

## Decision

- **Native-toolchain backends are opt-in, off by default.** Enabled per-tool via
  `[tools.<lang>.<tool>] enabled = true` in `poly.toml` (ADR 0013). Missing toolchains
  are never errors; the language falls through to tier-2 generic formatting (ADR 0004).
- **Single-file, stdin→stdout tools only.** Wrap tools that process one file per
  invocation over stdin (gofmt, rustfmt, zig fmt). Project-wide tools (clippy, go vet,
  mix format) do NOT fit the rayon per-file unit and are explicitly out of scope.
- **Capability-gated, graceful degradation.** Probe for the tool once and cache the
  result; declare the `format` / `lint` capability only when the tool is found AND
  enabled. If absent, no error — just lower fidelity via tier-2 fallback.
- **Per-file, stdin/stdout, no shell.** Invoke with a fixed argv, content fed on stdin.
  Never compose a shell command string from user input; use `execvp` (or Rust's
  `Command`) directly to avoid injection.
- **Honest cache key.** Fold the tool's resolved `--version` into the cache key
  (ADR 0008). A toolchain upgrade invalidates cached results automatically. This also
  means `version()` for a native-toolchain engine changes when the tool's version does.
- **Tool-specific interops:** Documented scoped interops for Cargo (clippy, cargo sort,
  cargo machete) and Go (gofmt). **Deferred:** golangci-lint (project-wide model doesn't
  fit per-file rayon; see rationale below).

## Consequences

Positive:

- Users who have a language's toolchain installed and want first-party behavior can opt
  in and get exactly that.
- Zero-dependency guarantee holds for everyone by default; it's only an opt-in feature,
  like a native-optional dependency in Rust.
- Canonical tools stay canonical; we don't fork or reimplement them.

Negative / risks:

- Output varies based on the installed toolchain version. CI and dev must pin their
  toolchains consistently to avoid drift (standard practice for per-language CI, but
  re-introduces toolchain management burden for users who opt in).
- A tool that's not present (missing from `PATH` or not installed) silently falls back
  to tier-2. Debugging "why is my Go code being formatted differently?" requires
  awareness of the fallback mechanism.
- The set of natively-interopped tools grows as users request support; discipline is
  needed to avoid creep (project-wide tools, non-CLI tools, or tools with complex setup).

## Alternatives considered

- **Always use native toolchains when available:** rejected — it breaks the zero-dep
  guarantee for the default case; users who don't opt in should never see it.
- **Vendor pure-Rust reimplementations of all tools:** rejected — it's a maintenance
  sink and will never match the first-party behavior; tier-2 generic formatting is the
  right fallback.
- **Async hook runner to parallelize tool invocations:** rejected — the per-file rayon
  unit (ADR 0009) already saturates cores; subprocess spawn overhead is amortized.
- **Interop for golangci-lint / cargo-clippy / eslint:** rejected — these are
  project-wide analysis tools, not single-file formatters. They require a whole-workspace
  view and cannot be cached per-file. They are out of scope for the native-toolchain
  backend model. (golangci-lint is deferred; it may become a hook builtin in ADR 0013 if
  the use case emerges.)
