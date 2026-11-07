#!/usr/bin/env bash
#MISE description="Cross-compile a Go sidecar for one component triple (pure Go, reproducible)"
#USAGE arg "<name>" help="Sidecar under components/sidecars/"
#USAGE arg "<triple>" help="Rust target triple"
#USAGE arg "<out>" help="Absolute path of the executable to write"
# alef runs this as the `executable.build` of a producer_language = "executable" component; the output is
# exactly <out>. CGO is off, so one machine builds every triple, and -trimpath with an empty build id keeps
# the bytes the same wherever it runs.
#
#   mise run sidecar:build sh x86_64-unknown-linux-gnu "$PWD/target/sh"

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="${1:?usage: sidecar:build <name> <triple> <out>}"
TRIPLE="${2:?usage: sidecar:build <name> <triple> <out>}"
OUT="${3:?usage: sidecar:build <name> <triple> <out>}"

SRC="${ROOT_DIR}/components/sidecars/${NAME}"
[[ -f "${SRC}/go.mod" ]] || {
  echo "sidecar:build: no Go module at components/sidecars/${NAME}" >&2
  exit 1
}

case "${TRIPLE}" in
x86_64-unknown-linux-gnu) goos=linux goarch=amd64 ;;
aarch64-unknown-linux-gnu) goos=linux goarch=arm64 ;;
x86_64-apple-darwin) goos=darwin goarch=amd64 ;;
aarch64-apple-darwin) goos=darwin goarch=arm64 ;;
x86_64-pc-windows-msvc) goos=windows goarch=amd64 ;;
*)
  echo "sidecar:build: no GOOS/GOARCH for ${TRIPLE}" >&2
  exit 1
  ;;
esac

mkdir -p "$(dirname "${OUT}")"
cd "${SRC}"
CGO_ENABLED=0 GOOS="${goos}" GOARCH="${goarch}" go build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "${OUT}" .
