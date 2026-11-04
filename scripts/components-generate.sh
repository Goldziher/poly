#!/usr/bin/env bash
# Regenerate the alef outputs: the sql producer crate and the standalone host module.
#
# alef is the version pinned in mise.toml; set ALEF to use another binary.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

if [[ -n "${ALEF:-}" ]]; then
  "${ALEF}" generate --skip-compile
else
  mise x -- alef generate --skip-compile
fi
