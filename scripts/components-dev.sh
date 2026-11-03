#!/usr/bin/env bash
# Build the sql component for this host and write an unsigned dev lock pointing at it.
#
# Usage:
#   ./scripts/components-dev.sh
#   POLY_COMPONENT_DEV_LOCK="$PWD/target/components/components.lock.json" cargo test -p poly-component-host
#
# Debug build, unsigned package, file:// lock, all under target/components/. The lock has no public
# keys, so it must never be shipped. alef is the version pinned next to alef-component-abi in Cargo.toml;
# set ALEF to use another binary.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

ALEF_VERSION="0.118.0"
if [[ -n "${ALEF:-}" ]]; then
  alef() { "${ALEF}" "$@"; }
else
  alef() { mise x "github:xberg-io/alef@${ALEF_VERSION}" -- alef "$@"; }
fi

OUT="${ROOT_DIR}/target/components"
BUILD="${OUT}/build"
DIST="${OUT}/dist"
LOCK="${OUT}/components.lock.json"
# `alef component lock` also stages the lock into the tracked host module directory; the committed
# copy is the empty lock and must stay that way.
STAGED_LOCK="crates/poly-component-host/src/components.lock.json"

TARGET="$(rustc -vV | sed -n 's/^host: //p')"
[[ -n "${TARGET}" ]] || {
  echo "components:dev: cannot read the host triple from rustc -vV" >&2
  exit 1
}

rm -rf "${DIST}"
mkdir -p "${OUT}"
BACKUP="$(mktemp)"
trap 'rm -f "${BACKUP}"' EXIT
cp "${STAGED_LOCK}" "${BACKUP}"

alef component build --component sql --target "${TARGET}" --debug --target-dir "${BUILD}"

# `component package` has no --debug, so it is pointed at the debug library explicitly.
LIBRARY="$(find "${BUILD}/${TARGET}/debug" -maxdepth 1 -type f \
  \( -name 'libpoly_engines_sql_component.*' -o -name 'poly_engines_sql_component.dll' \) \
  \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) | head -n 1)"
[[ -n "${LIBRARY}" ]] || {
  echo "components:dev: no sql component library under ${BUILD}/${TARGET}/debug" >&2
  exit 1
}

alef component package --unsigned --component sql --target "${TARGET}" --library "${LIBRARY}" --output "${DIST}"
# `component lock` wants a record for every component and target alef.toml lists and has neither
# filter, so the dev lock is written against a copy of the config narrowed to the sql component and
# the host triple.
DEV_CONFIG="${OUT}/alef.dev.toml"
python3 - "${TARGET}" "${DEV_CONFIG}" <<'PY'
import re
import sys

text = open("alef.toml", encoding="utf-8").read()
sections = re.split(r"(?m)^(?=\[)", text)
kept = [s for s in sections if not s.startswith("[[crates.components]]") or re.search(r'(?m)^name = "sql"$', s)]
if sum(s.startswith("[[crates.components]]") for s in kept) != 1:
    sys.exit("components:dev: expected exactly one sql [[crates.components]] block in alef.toml")
narrowed, count = re.subn(r"targets = \[[^\]]*\]", f'targets = ["{sys.argv[1]}"]', "".join(kept), flags=re.S)
if count == 0:
    sys.exit("components:dev: no `targets = [...]` in alef.toml")
open(sys.argv[2], "w", encoding="utf-8").write(narrowed)
PY
alef --config "${DEV_CONFIG}" component lock --unsigned --input "${DIST}" --output "${LOCK}" \
  --url-template "file://${DIST}/{artifact}"

cp "${BACKUP}" "${STAGED_LOCK}"

ARCHIVES="$(find "${DIST}" -maxdepth 1 -name '*.tar.gz' | wc -l | tr -d ' ')"
[[ "${ARCHIVES}" == "1" ]] || {
  echo "components:dev: expected 1 archive in ${DIST}, found ${ARCHIVES}" >&2
  exit 1
}
echo "dev lock: ${LOCK}"
echo "export POLY_COMPONENT_DEV_LOCK=${LOCK}"
