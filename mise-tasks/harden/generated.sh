#!/usr/bin/env bash
#MISE description="Run the harness against generated-code repositories (rule-audit input)"
#USAGE arg "[roots]..." help="Named roots only"
set -euo pipefail

eval "set -- ${usage_roots:-}"
exec mise run harden --corpus c "$@"
