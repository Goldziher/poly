#!/usr/bin/env bash
#MISE description="Cross-compile a Go sidecar for component triples (pure Go, reproducible)"
#USAGE arg "<name>" help="Sidecar under components/sidecars/"
#USAGE arg "[triples]..." help="Target triples (default: the host's)"
#USAGE flag "--out <dir>" help="Output directory" default="target/sidecars"
# Writes <out>/<name>/<triple>/<name>[.exe]. CGO is off, so one machine builds every triple, and
# -trimpath with an empty build id keeps the bytes the same wherever it runs.
#
#   mise run sidecar:build sh                                   the host triple
#   mise run sidecar:build sh x86_64-unknown-linux-gnu aarch64-apple-darwin
#
# Prints one path per triple. The components' release triples are the ones alef.toml lists.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="${usage_name:?usage: mise run sidecar:build <name> [triples]...}"
OUT="${usage_out:-target/sidecars}"
[[ "${OUT}" == /* ]] || OUT="${ROOT_DIR}/${OUT}"
eval "set -- ${usage_triples:-}"
TRIPLES=("$@")
[[ ${#TRIPLES[@]} -gt 0 ]] || TRIPLES=("$(rustc -vV | sed -n 's/^host: //p')")

SRC="${ROOT_DIR}/components/sidecars/${NAME}"
[[ -f "${SRC}/go.mod" ]] || {
  echo "sidecar:build: no Go module at components/sidecars/${NAME}" >&2
  exit 1
}

for triple in "${TRIPLES[@]}"; do
  ext=""
  case "${triple}" in
  x86_64-unknown-linux-gnu) goos=linux goarch=amd64 ;;
  aarch64-unknown-linux-gnu) goos=linux goarch=arm64 ;;
  x86_64-apple-darwin) goos=darwin goarch=amd64 ;;
  aarch64-apple-darwin) goos=darwin goarch=arm64 ;;
  x86_64-pc-windows-msvc) goos=windows goarch=amd64 ext=".exe" ;;
  *)
    echo "sidecar:build: no GOOS/GOARCH for ${triple}" >&2
    exit 1
    ;;
  esac
  dest="${OUT}/${NAME}/${triple}/${NAME}${ext}"
  mkdir -p "$(dirname "${dest}")"
  (cd "${SRC}" && CGO_ENABLED=0 GOOS="${goos}" GOARCH="${goarch}" \
    go build -trimpath -ldflags="-s -w -buildid=" -o "${dest}" .)
  echo "${dest}"
done
