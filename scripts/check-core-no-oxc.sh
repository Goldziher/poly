#!/usr/bin/env bash
# Fail when the core binary (`poly-cli --no-default-features`) links any oxc crate.
#
# The stat fast path and cache lookups are served by host-side code, so the core must build and run
# without the web engine family. An `oxc_*` crate in this tree means a host-side module grew an
# engine dependency again.
set -euo pipefail
shopt -s inherit_errexit

tree="$(cargo tree --locked -p poly-cli --no-default-features -e normal --prefix none)"
# The tree must be non-empty and name the root, or a failed resolution would read as "no oxc".
grep -q '^poly-cli ' <<<"$tree" || {
  echo "check-core-no-oxc: cargo tree did not list poly-cli; refusing to report a pass" >&2
  exit 2
}
offenders="$(grep '^oxc_' <<<"$tree" | sort -u || true)"
count="$(grep -c . <<<"$offenders" || true)"
if [[ "$count" -ne 0 ]]; then
  echo "check-core-no-oxc: $count oxc crate(s) in the core dependency tree:" >&2
  echo "$offenders" >&2
  exit 1
fi
echo "check-core-no-oxc: no oxc crates in the core dependency tree"
