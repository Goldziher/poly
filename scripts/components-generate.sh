#!/usr/bin/env bash
# Regenerate the alef outputs: the sql producer crate and the standalone host module.
#
# alef 0.118.0 also rewrites the version of every workspace crate that carries its own (gitfluff) and
# seeds a rust-toolchain.toml; both are undone here because poly wants neither.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

KEEP=(Cargo.lock crates/gitfluff/Cargo.toml)
BACKUP="$(mktemp -d)"
trap 'rm -rf "${BACKUP}"' EXIT
for file in "${KEEP[@]}"; do
  mkdir -p "${BACKUP}/$(dirname "${file}")"
  cp "${file}" "${BACKUP}/${file}"
done

ALEF_VERSION="0.118.0"
if [[ -n "${ALEF:-}" ]]; then
  "${ALEF}" generate --skip-compile
else
  mise x "github:xberg-io/alef@${ALEF_VERSION}" -- alef generate --skip-compile
fi

for file in "${KEEP[@]}"; do
  cp "${BACKUP}/${file}" "${file}"
done
rm -f rust-toolchain.toml
