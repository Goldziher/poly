#!/usr/bin/env bash
#MISE description="Gate: the core binary and the root workspace link no oxc, rmcp or tokio crate"
# Fail when the released core binary (`poly-cli --no-default-features --features core`) links any oxc crate, or the MCP
# server stack (`rmcp`, `tokio`), or when any crate of the root workspace, with every feature on,
# depends on an oxc crate.
#
# The stat fast path and cache lookups are served by host-side code, so the core must build and run
# without the web engine family. An `oxc_*` crate in this tree means a host-side module grew an
# engine dependency again. The MCP server is the `mcp` frontend component, so `rmcp` or `tokio` here
# means the core links it again. The web family and its git-pinned oxc crates live in the separate
# `components/` workspace, so no oxc crate may appear anywhere in the root workspace's dependency tree.
set -euo pipefail
shopt -s inherit_errexit

tree="$(cargo tree --locked -p poly-cli --no-default-features --features core -e normal --prefix none)"
# The tree must be non-empty and name the root, or a failed resolution would read as "no oxc".
grep -q '^poly-cli ' <<<"$tree" || {
  echo "check-core-no-oxc: cargo tree did not list poly-cli; refusing to report a pass" >&2
  exit 2
}
offenders="$(grep -E '^(oxc_|rmcp |tokio )' <<<"$tree" | sort -u || true)"
count="$(grep -c . <<<"$offenders" || true)"
if [[ "$count" -ne 0 ]]; then
  echo "check-core-no-oxc: $count forbidden crate(s) in the core dependency tree:" >&2
  echo "$offenders" >&2
  exit 1
fi

workspace_tree="$(cargo tree --locked --workspace --all-features -e normal --prefix none)"
# Non-empty and naming the core, or a failed resolution would read as "no oxc".
grep -q '^poly-core ' <<<"$workspace_tree" || {
  echo "check-core-no-oxc: cargo tree did not list poly-core for the workspace; refusing to report a pass" >&2
  exit 2
}
workspace_offenders="$(grep -E '^oxc_' <<<"$workspace_tree" | sort -u || true)"
workspace_count="$(grep -c . <<<"$workspace_offenders" || true)"
if [[ "$workspace_count" -ne 0 ]]; then
  echo "check-core-no-oxc: $workspace_count oxc crate(s) in the root workspace dependency tree:" >&2
  echo "$workspace_offenders" >&2
  exit 1
fi
echo "check-core-no-oxc: no oxc, rmcp or tokio crates in the core dependency tree, no oxc crates in the workspace"
