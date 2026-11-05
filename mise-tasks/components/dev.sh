#!/usr/bin/env bash
#MISE description="Build components (debug), package them unsigned and write a file:// dev lock under target/components/"
#USAGE arg "[components]..." help="Components to build (default: sql)"
# Build components for this host and write an unsigned dev lock pointing at them.
#
# Usage:
#   mise run components:dev            # the sql component
#   mise run components:dev sql ruby   # any configured components
#   POLY_COMPONENT_DEV_LOCK="$PWD/target/components/components.lock.json" cargo test -p poly-component-host
#
# Debug build, unsigned package, file:// lock, all under target/components/. The lock has no public
# keys, so it must never be shipped. alef is the version pinned in mise.toml; set ALEF to use another binary.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

# `command` so an unset ALEF reaches the alef binary rather than recursing into this function.
alef() { command "${ALEF:-alef}" "$@"; }

COMPONENTS=("$@")
[[ ${#COMPONENTS[@]} -gt 0 ]] || COMPONENTS=(sql)
SELECTED="$(
  IFS=,
  echo "${COMPONENTS[*]}"
)"

OUT="${ROOT_DIR}/target/components"
BUILD="${OUT}/build"
DIST="${OUT}/dist"
LOCK="${OUT}/components.lock.json"

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "${TARGET}" ]] || {
  echo "components:dev: cannot read the host triple from rustc -vV" >&2
  exit 1
}

rm -rf "${DIST}"
mkdir -p "${OUT}"

alef component build --component "${SELECTED}" --target "${TARGET}" --debug --target-dir "${BUILD}"
alef component package --unsigned --debug --component "${SELECTED}" --target "${TARGET}" --target-dir "${BUILD}" \
  --output "${DIST}"
# --component keeps this partial dev lock out of the tracked host module directory.
alef component lock --unsigned --component "${SELECTED}" --target "${TARGET}" --input "${DIST}" --output "${LOCK}" \
  --url-template "file://${DIST}/{artifact}"

ARCHIVES="$(find "${DIST}" -maxdepth 1 -name '*.tar.gz' | wc -l | tr -d ' ')"
[[ "${ARCHIVES}" == "${#COMPONENTS[@]}" ]] || {
  echo "components:dev: expected ${#COMPONENTS[@]} archive(s) in ${DIST}, found ${ARCHIVES}" >&2
  exit 1
}
echo "dev lock: ${LOCK}"
echo "export POLY_COMPONENT_DEV_LOCK=${LOCK}"
