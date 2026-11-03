#!/usr/bin/env bash
# Run poly-core in components mode and check the sql component against the in-process snapshots.
#
# Usage:
#   mise run components:dev && mise run components:parity
#
# Components mode is the default engine families minus `engine-sql`, plus `dev-lock`, so the sql
# family can only come from the component named by the dev lock. Needs the lock that
# `scripts/components-dev.sh` writes; fails, never skips, when it is missing.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

export POLY_COMPONENT_DEV_LOCK="${POLY_COMPONENT_DEV_LOCK:-${CARGO_TARGET_DIR:-${ROOT_DIR}/target}/components/components.lock.json}"
[[ -f "${POLY_COMPONENT_DEV_LOCK}" ]] || {
  echo "components:parity: no dev lock at ${POLY_COMPONENT_DEV_LOCK}; run 'mise run components:dev' first" >&2
  exit 1
}

# `engine-sql` is the only family left out; every other engine family stays linked.
FEATURES="dev-lock,engine-web,engine-python,engine-php,engine-biome,engine-ruby,engine-nix,engine-typos,engine-quality,engine-astgrep,engine-uncomment,engine-dockerfile,engine-dotenv,engine-ini,engine-hcl,engine-graphql"

cargo test -p poly-core --locked --no-default-features --features "${FEATURES}" --no-fail-fast \
  --test component_parity --test components
