#!/usr/bin/env bash
#MISE description="Build and package the sh sidecar, then run the host<->sidecar conformance tests against it"
# Builds the sidecar for the host, packages it the way a release does (alef component package --library),
# writes an unsigned dev lock and runs poly-component-host's sidecar tests with the real executable. The
# tests compare its output with the host shfmt, so shfmt must be on PATH (mise installs it).
#
# POLY_REQUIRE_SIDECAR=1 turns a skipped test into a failure: CI sets it, so a conformance run that
# silently skipped everything cannot pass. Set CONFORMANCE_REQUIRED=0 to run without that guard.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
OUT="${ROOT_DIR}/target/sidecars/conformance"
rm -rf "${OUT}"
mkdir -p "${OUT}"

EXE="$(mise run sidecar:build sh "${TARGET}" --out "${OUT}/bin" | tail -1)"
[[ -x "${EXE}" ]] || {
  echo "sidecar:conformance: the build produced no executable at '${EXE}'" >&2
  exit 1
}

alef component package --unsigned --component sh --target "${TARGET}" --library "${EXE}" --output "${OUT}/dist"
alef component lock --unsigned --component sh --target "${TARGET}" --input "${OUT}/dist" \
  --output "${OUT}/components.lock.json" --url-template "file://${OUT}/dist/{artifact}"

export POLY_SH_SIDECAR="${EXE}"
export POLY_SH_SIDECAR_LOCK="${OUT}/components.lock.json"
[[ "${CONFORMANCE_REQUIRED:-1}" == "0" ]] || export POLY_REQUIRE_SIDECAR=1
cargo test -p poly-component-host --test sidecar --no-fail-fast -- --nocapture
