# 0045 — Sidecar Components: Non-Rust Engines as Out-of-Process Executables

- Status: Accepted
- Date: 2026-11-06
- Amends: ADR 0002, ADR 0039
- Related: ADR 0038

## Context

ADR 0039 ships engines as signed dylib components that share the host process. That fits Rust, but not
an engine written in Go: every Go `c-shared` library carries its own Go runtime and several in one
process are unsupported, and a Go engine would otherwise have to be rewritten or kept as a host tool
poly shells out to. Shell formatting is the case at hand. `shfmt` is `mvdan.cc/sh/v3`, a Go library,
and poly's native-toolchain engine (ADR 0014) runs the `shfmt` binary when it is on `PATH`, so its
output depends on a tool the user installed.

alef 0.127 adds a framed stdio transport (alef ADR-0017) and 0.128 builds executable components (alef ADR-0023): a signed executable payload the host spawns
once and calls with `invoke_bytes(method, payload)`. Issue #83 reserved a `sidecar` component kind for
this.

## Decision

A component whose index entry says `kind = "sidecar"` is an executable, not a dylib, and speaks poly's
existing MessagePack protocol (ADR 0039) over alef's framed stdio.

- **Same protocol, different carrier.** `describe()` is `invoke_bytes("describe", [])`. `call(header,
  body)` is `invoke_bytes("call", payload)` with `payload = u32_le(len(header)) ++ header ++ body`
  (`poly_component_protocol::sidecar`, and the `ProtocolSchema` documentation in
  `schemas/component-protocol.json`). The proxy `ComponentEngine` is unchanged; only the transport
  differs. A host that lists a component in `HostOptions::sidecars` also selects it with
  `ComponentManager::with_stdio_component`.
- **Failure modes are the existing ones.** A timeout (30 s by default, `sidecar_timeout`), a crash
  mid-call, a malformed frame, a protocol-version mismatch and a `describe()` that differs from the
  index entry each fail the call or the load with a component error, and the failure is remembered for
  the run. A dead sidecar is never restarted and never replaced by another engine: a missing or failed
  component is exit 2, as for a dylib.
- **Identity comes from the verified manifest.** alef requires the stdio handshake to repeat the
  signed manifest's component version, feature hash and contract hash. A Go executable has no way to
  embed those at build time without changing its bytes every release, so the sidecar reads
  `component.json` from its own directory, where alef installs the manifest it verified (size,
  SHA-256, signature) before spawning it. The executable carries no version of its own.
- **`sh` is the first sidecar.** `components/sidecars/sh` is a pure-Go program (`CGO_ENABLED=0`) that
  formats shell with `mvdan.cc/sh/v3/syntax` in the sidecar's own process: no `shfmt` binary, no
  subprocess. It takes the options poly already passes the host tool: the indent width and `use_tabs`
  (`-i 0`), and picks bash, POSIX, mksh, bats or zsh from the file name and shebang as `shfmt` does
  when it is given a file. Output is byte-identical to `shfmt` 3.14.1 for the same options; the golden
  files in `components/sidecars/sh/testdata/golden` are that binary's output, and a Rust test compares
  the two on every fixture.
- **Engine name `shfmt`.** `[fmt.shell.shfmt]` keeps working; `shellcheck` stays a native-toolchain
  engine. `.editorconfig` resolution moved from the native engine into a host hook
  (`HostHook::Editorconfig`), so the host resolves the per-file indent and cache salt and the sidecar,
  which sees only the bytes, never reads it.
- **Precedence.** When the `sh` component is selected it is the shell formatter, and a component engine
  replaces a built-in engine of the same name. `[components] disable = ["sh"]` selects the host `shfmt`
  instead, and that is the only way: the host tool is an explicit opt-in. `parse error` leaves a file
  alone, as the host tool's non-zero exit does.
- **Bundled builds keep the host tool.** A Go program cannot be linked into the static musl binary, and
  shipping a second executable next to it would make "bundled" mean "bundled, plus a file that must
  stay beside it". The bundled build (`engine-shfmt` in `all-engines`) therefore treats `sh` as linked
  and formats shell with the host `shfmt` when present and the tree-sitter tier otherwise, as before.
  Every other build downloads `sh`.
- **Build and release.** `alef.toml` declares `sh` with `producer_language = "executable"`: the
  `executable.build` command runs `mise-tasks/sidecar/build.sh`, which cross-compiles the five component
  triples with `CGO_ENABLED=0 -trimpath -ldflags="-s -w -buildid="` and writes exactly `{out}`. alef plans,
  builds, packages, signs and locks it with the Rust components, so one lock covers both and an unchanged
  sidecar is reused across releases; the fingerprint covers `components/sidecars/sh`, the build script and
  the output of `go version`. alef generates no producer or bridge for it.

Amendment to ADR 0002: a downloaded component may now be an executable that runs as a child process,
not only a library loaded into poly. This is a third scoped exception beside native-toolchain
backends and the hook runner, with the same trust: the SHA-256 in the embedded lock is the root, the
archive is verified before it is spawned, and poly never runs an executable it did not verify.

## Consequences

- Engines can be written in a language that cannot be a Rust dylib, behind the same contract and the
  same `Engine` proxy.
- Shell formatting no longer depends on a host `shfmt`; a core build gets the same output on every
  machine. Cost: one more 1.4 MB download for a repository with shell files.
- Calls to one sidecar are serialized by alef's transport, so formatting shell does not scale across
  the worker pool. Per-file cost is microseconds against the process round trip, and a sidecar that
  needs more can be run as several components later.
- The component displaces the tree-sitter reindenter for shell before `enabled` is applied, so
  `[fmt.shell.shfmt] enabled = false` no longer falls back to it; `[components] disable = ["sh"]`
  keeps the host-tool path, with its tree-sitter fallback.
- `batch` is no longer a `[fmt.shell.shfmt]` key: a long-lived process has no start-up cost to
  amortize.
- The Go toolchain must be on `PATH` wherever alef builds the `sh` component, including the release
  containers, and `lock --reuse` does not re-verify the toolchain (alef ADR-0023).

## Alternatives considered

- **Go `c-shared` loaded in process.** Rejected: one Go runtime per process, and poly may load several
  components.
- **Keep shelling out to a host `shfmt`.** Rejected: output depends on the installed version, which is
  what ADR 0039's "same bytes on every machine" rule exists to prevent.
- **Port shfmt to Rust.** Rejected: the shell grammar and printer are large, and bytewise parity with
  `shfmt` is the requirement.
- **Embed the Go sidecar in the bundled binary.** Rejected: it would be written to disk and spawned,
  which is a download with extra steps and a larger core.
