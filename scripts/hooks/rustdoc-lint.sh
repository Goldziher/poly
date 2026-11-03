#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  printf 'rustdoc-lint: skipping — `cargo` not found on PATH.\n' >&2
  exit 0
fi

FEATURES="${RUSTDOC_FEATURES:---all-features}"

export RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links"

# alef-generated component producer crates carry no docs (xberg-io/alef generator output).
exec cargo doc --workspace --exclude poly-engines-sql-component --exclude poly-engines-mcp-component --no-deps --quiet "$FEATURES"
