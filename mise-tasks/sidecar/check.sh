#!/usr/bin/env bash
#MISE description="Gate the Go sidecars: vet, lint, race tests, vulnerabilities, licences, go.mod tidiness"
#USAGE arg "[steps]..." help="Steps to run (default: all)" { choices "vet" "lint" "test" "vuln" "licenses" "tidy" }
# Runs from components/sidecars/<name>; today that is `sh`. Each step fails the task on its own.
#
#   mise run sidecar:check            every step
#   mise run sidecar:check test tidy  just those

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STEPS=("$@")
[[ ${#STEPS[@]} -gt 0 ]] || STEPS=(vet lint test vuln licenses tidy)
# Licences the module graph may carry; mvdan.cc/sh is BSD-3-Clause, vmihailenco/msgpack BSD-2-Clause.
ALLOWED_LICENSES="BSD-3-Clause,BSD-2-Clause,MIT,Apache-2.0,ISC"

for dir in "${ROOT_DIR}"/components/sidecars/*/; do
  [[ -f "${dir}go.mod" ]] || continue
  echo "== ${dir#"${ROOT_DIR}"/}"
  cd "${dir}"
  for step in "${STEPS[@]}"; do
    echo "-- ${step}"
    case "${step}" in
    vet) go vet ./... ;;
    lint) golangci-lint run ./... ;;
    test) CGO_ENABLED=1 go test -race -count=1 ./... ;;
    vuln) govulncheck ./... ;;
    licenses) go-licenses check ./... --allowed_licenses="${ALLOWED_LICENSES}" --ignore "$(go list -m)" ;;
    tidy) go mod tidy -diff ;;
    esac
  done
done
