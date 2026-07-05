# 0014 — Toolchain Interop: Capability-Probed First-Party CLIs

- Status: Accepted
- Date: 2026-06-28
- Updated: 2026-07-05 (project-wide tools deferred here — `cargo clippy`, `pyrefly`,
  `golangci-lint` — now have a home as whole-workspace hooks under ADR 0019, not as
  per-file native-toolchain backends)

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

- **Canonical formatters are default-on when present (amendment, 2026-06-28).** The
  first-party formatters with no viable Rust library — `rustfmt` (Rust) and `gofmt`
  (Go) — format their languages **by default when detected on `PATH`**, winning over the
  tier-2 generic tier. This supersedes the original opt-in-off default for these two
  tools: measured tier-2 Rust formatting was both the slowest path and ~67%
  would-change churn against `rustfmt`, so honoring the canonical tool when available is
  the better default. When the tool is absent, the language falls through to tier-2 and an
  **info-level** notice is emitted once per language per run; absence is never an error, so
  the zero-system-dependency guarantee still holds for anyone without the toolchain. A user
  can still force a canonical tool off with `[fmt.<lang>.<tool>] enabled = false`.
- **Other native-toolchain backends remain opt-in, off by default.** Tools other than the
  canonical formatters above (e.g. `zig fmt`) are enabled per-tool via
  `[fmt.<lang>.<tool>] enabled = true` in `poly.toml` (ADR 0013). Missing toolchains
  are never errors; the language falls through to tier-2 generic formatting (ADR 0004).
- **Single-file, stdin→stdout tools only.** Wrap tools that process one file per
  invocation over stdin (gofmt, rustfmt, zig fmt). Project-wide tools (clippy, go vet,
  mix format) do NOT fit the rayon per-file unit and are out of scope **for this
  native-toolchain backend model** — but they are no longer homeless: they run as
  whole-workspace hooks with staged isolation (ADR 0019).
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

- **Always use native toolchains when available:** rejected in general, but **adopted for
  the canonical formatters** (`rustfmt`, `gofmt`) in the 2026-06-28 amendment — these have
  no viable Rust library and tier-2 was demonstrably lower fidelity, so default-on-when-
  present is correct for them. The zero-dep guarantee is preserved because absence is not an
  error (info notice + tier-2 fallback), and a user can still force them off. Other tools
  stay opt-in.
- **Vendor pure-Rust reimplementations of all tools:** rejected — it's a maintenance
  sink and will never match the first-party behavior; tier-2 generic formatting is the
  right fallback.
- **Async hook runner to parallelize tool invocations:** rejected — the per-file rayon
  unit (ADR 0009) already saturates cores; subprocess spawn overhead is amortized.
- **Interop for golangci-lint / cargo-clippy / eslint:** rejected **for the per-file
  native-toolchain backend model** — these are project-wide analysis tools, not single-file
  formatters, requiring a whole-workspace view that cannot be cached per-file. They are
  instead run as **whole-workspace hooks with staged isolation and whole-tree result
  caching (ADR 0019)**: `cargo clippy` ships as a `cargo` group builtin, and `pyrefly` /
  `golangci-lint` are inline `workspace = true` jobs. This supersedes the earlier "deferred /
  may become a hook builtin" note.
