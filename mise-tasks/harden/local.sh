#!/usr/bin/env bash
#MISE description="Run the harness against the sibling working trees next door (read-only)"
#USAGE arg "[roots]..." help="Named roots only"
set -euo pipefail

eval "set -- ${usage_roots:-}"
exec mise run harden --corpus a "$@"
