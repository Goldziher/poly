#!/usr/bin/env bash
#MISE description="Build and package the sh sidecar with alef, then run the host<->sidecar conformance tests against it"
# Builds the sidecar for the host the way a release does (alef component build/package/lock on the
# producer_language = "executable" component), writes an unsigned dev lock and runs poly-component-host's
# sidecar tests with the real executable. The tests compare its output with the host shfmt, so shfmt must be
# on PATH (mise installs it).
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

alef component plan --component sh --target "${TARGET}" --output "${OUT}/plan.json"
alef component build --component sh --target "${TARGET}" --plan "${OUT}/plan.json"
alef component package --unsigned --component sh --target "${TARGET}" --plan "${OUT}/plan.json" --output "${OUT}/dist"
alef component lock --unsigned --component sh --target "${TARGET}" --input "${OUT}/dist" \
  --output "${OUT}/components.lock.json" --url-template "file://${OUT}/dist/{artifact}"

EXE="$(tar -tzf "${OUT}"/dist/poly-component-sh-*.tar.gz | grep -E '(^|/)sh(\.exe)?$' | head -1)"
[[ -n "${EXE}" ]] || {
  echo "sidecar:conformance: the package holds no sh executable" >&2
  exit 1
}
mkdir -p "${OUT}/unpacked"
tar -xzf "${OUT}"/dist/poly-component-sh-*.tar.gz -C "${OUT}/unpacked"
EXE="${OUT}/unpacked/${EXE}"
[[ -x "${EXE}" ]] || {
  echo "sidecar:conformance: '${EXE}' is not executable" >&2
  exit 1
}

export POLY_SH_SIDECAR="${EXE}"
export POLY_SH_SIDECAR_LOCK="${OUT}/components.lock.json"
[[ "${CONFORMANCE_REQUIRED:-1}" == "0" ]] || export POLY_REQUIRE_SIDECAR=1
cargo test -p poly-component-host --test sidecar --no-fail-fast -- --nocapture
