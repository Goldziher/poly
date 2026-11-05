#!/usr/bin/env bash
#MISE description="Gate: components/Cargo.lock is current for the root crates components/ builds by path"
# components/ depends on root crates (poly-engines, poly-engine-api, ...) by path, so a dependency
# change in one of them stales components/Cargo.lock without any file under components/ changing.
# CI builds components/ with --locked, which then fails; this catches it at commit time.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if ! cargo metadata --locked --format-version 1 --manifest-path "${root}/components/Cargo.toml" >/dev/null; then
  echo "check:components-lock: components/Cargo.lock is stale; refresh it with" >&2
  echo "  cargo metadata --format-version 1 --manifest-path components/Cargo.toml > /dev/null" >&2
  exit 1
fi
