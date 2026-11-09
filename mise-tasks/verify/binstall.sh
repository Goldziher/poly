#!/usr/bin/env bash
# MISE description="Assert cargo binstall resolves the release archive for every shipped triple"
# USAGE arg "[manifest]" help="Path to the poly-cli crate whose binstall metadata is checked" default="crates/poly-cli"
set -euo pipefail

manifest="${usage_manifest:-${1:-crates/poly-cli}}"
# The triples a release ships an archive for that binstall can resolve.
targets=(aarch64-apple-darwin x86_64-unknown-linux-gnu x86_64-unknown-linux-musl x86_64-pc-windows-msvc
  aarch64-unknown-linux-gnu)

failed=0
for target in "${targets[@]}"; do
  log="$(cargo binstall --dry-run --no-confirm --disable-strategies compile,quick-install \
    --manifest-path "$manifest" --targets "$target" poly-cli 2>&1)" || true
  if grep -q "($target) has been downloaded from github.com" <<<"$log"; then
    echo "ok   $target"
  else
    echo "FAIL $target"
    printf '%s\n' "$log" | tail -5
    failed=1
  fi
done
exit "$failed"
