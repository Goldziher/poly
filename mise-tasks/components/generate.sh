#!/usr/bin/env bash
#MISE description="Regenerate the alef component producer crate and host module (alef.toml)"
# Regenerate the alef outputs: the sql producer crate and the standalone host module.
#
# alef is the version pinned in mise.toml; set ALEF to use another binary.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT_DIR}"

"${ALEF:-alef}" generate --skip-compile
