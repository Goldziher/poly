#!/usr/bin/env bash
#MISE description="Gate: the released core binary (profile `core`) stays under 10 MiB"
#USAGE arg "[binary]" help="Check this built binary instead of building one"
#USAGE flag "--limit <bytes>" help="Size ceiling in bytes" default="10485760"
# Build the core exactly as the release does (`poly-cli --no-default-features --features core`,
# `--profile core`) and fail when the stripped binary is larger than the ceiling (ADR 0039, ADR 0043).
# With a path, check that binary instead: the release workflow checks the one it ships. The ceiling is
# meant for x86_64-unknown-linux-gnu, the largest of the native triples.
set -euo pipefail
shopt -s inherit_errexit

limit="${usage_limit:-10485760}"
bin="${usage_binary:-}"
if [[ -z "$bin" ]]; then
  cargo build --locked --profile core -p poly-cli --no-default-features --features core
  bin="${CARGO_TARGET_DIR:-target}/core/poly"
fi
[[ -f "$bin" ]] || {
  echo "check-core-size: no binary at $bin; refusing to report a pass" >&2
  exit 2
}
size="$(wc -c <"$bin" | tr -d ' ')"
# An empty or truncated file would pass any ceiling.
[[ "$size" -gt 1000000 ]] || {
  echo "check-core-size: $bin is only $size bytes; refusing to report a pass" >&2
  exit 2
}
echo "check-core-size: $bin is $size bytes (ceiling $limit)"
if [[ "$size" -gt "$limit" ]]; then
  echo "check-core-size: the core is $((size - limit)) bytes over the ceiling" >&2
  exit 1
fi
