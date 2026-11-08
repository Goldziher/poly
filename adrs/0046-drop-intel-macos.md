# 0046 — Drop the Intel macOS Release Target

- Status: Accepted
- Date: 2026-11-08
- Related: ADR 0039, ADR 0045

## Context

poly's release builds six platform archives, and every component family is built once per target.
`x86_64-apple-darwin` is the slowest and least available of them: it needs the `macos-26-intel`
runner, and its component builds take about 85 minutes, the long pole of the release. v0.33.0 was never
published because that build failed. Apple has moved the Mac line to Apple silicon, and Intel Macs are
a shrinking, end-of-life share of the install base.

## Decision

poly ships five platform archives: Linux gnu x86_64 and aarch64, Linux musl x86_64, macOS aarch64 and
Windows x86_64. Starting with the next release there is no `x86_64-apple-darwin` archive, component
build, sidecar build, `@goldziher/polylint-darwin-x64` npm package or `macosx_10_12_x86_64` wheel. The
Homebrew formula declares `depends_on arch: :arm64` on macOS. `install.sh` stops with a message on
Intel macOS. Intel Mac users build from source.

Rosetta does not help: it runs x86_64 binaries on Apple silicon, not the reverse, so an Intel Mac cannot
run the aarch64 archive.

## Consequences

- Positive: the release no longer waits on the Intel runner, one fewer npm package and wheel to keep in
  lock-step, a smaller component matrix.
- Negative: Intel Mac users have no prebuilt binary, and already published releases keep their Intel
  archives but get no update. The already published `@goldziher/polylint-darwin-x64` stays on the
  registry; it is not unpublished.

## Alternatives considered

- Keep the target but build it only for core, not components: still needs the Intel runner and leaves
  those users with a binary whose components cannot load.
- Cross-compile x86_64 macOS from an Apple silicon runner: would remove the runner dependency but keeps
  an untested target and the packages around it.
