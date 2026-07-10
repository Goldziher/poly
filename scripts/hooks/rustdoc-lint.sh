#!/usr/bin/env bash
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  printf 'rustdoc-lint: skipping — `cargo` not found on PATH.\n' >&2
  exit 0
fi

FEATURES="${RUSTDOC_FEATURES:---all-features}"

export RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links"

exec cargo doc --workspace --no-deps --quiet "$FEATURES"
