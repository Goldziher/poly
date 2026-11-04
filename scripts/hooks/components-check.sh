#!/usr/bin/env bash
# Whole-workspace checks for the `components/` cargo workspace, which the root workspace excludes so
# the root `cargo clippy` / `cargo deny` / `cargo doc` never see it.
#
# Usage: components-check.sh <clippy|deny|rustdoc>
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "components-check: skipping, cargo not found on PATH." >&2
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="${root}/components/Cargo.toml"

case "${1:-}" in
clippy)
  exec cargo clippy --manifest-path "$manifest" --workspace --all-targets -- -D warnings
  ;;
deny)
  exec cargo deny --manifest-path "$manifest" --config "${root}/components/deny.toml" check
  ;;
rustdoc)
  export RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links"
  # alef-generated component producer crates carry no docs (xberg-io/alef generator output).
  exec cargo doc --manifest-path "$manifest" --workspace --exclude poly-engines-web-component \
    --exclude poly-engines-biome-component --exclude poly-engines-ruby-component --no-deps --quiet --all-features
  ;;
*)
  echo "usage: components-check.sh <clippy|deny|rustdoc>" >&2
  exit 2
  ;;
esac
