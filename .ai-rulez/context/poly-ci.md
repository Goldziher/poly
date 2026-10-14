---
type: Concept
title: Poly CI
x-ai-rulez:
  kind: context
  id: poly-ci
  metadata:
    priority: high
---

## CI

`.github/workflows/ci.yaml` runs five jobs on push/PR to `main`: `cargo fmt --all --check`;
`cargo clippy --workspace --exclude conformance --all-targets -- -D warnings` and
`cargo test --workspace --exclude conformance --no-fail-fast`, both on a Linux/macOS/Windows
matrix; `dogfood`, which runs the binary against poly's own repo (`poly lint --no-workspace .`
then `poly fmt --check .`) on Linux and Windows; and `cargo-deny check`.
`.github/workflows/publish.yaml` builds and uploads the release artifacts.

The hardening harness (`task harden`, `scripts/harden.sh`) is **not** a CI job. It clones large
third-party trees and its per-rule counts inform a decision rather than pass or fail a commit, so
it is run deliberately — before a release, or when a rule's severity is in question.

Run `poly fmt --check .` and `poly lint .` after changes to verify compliance.
