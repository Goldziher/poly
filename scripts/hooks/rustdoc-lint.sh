#!/usr/bin/env bash
# rustdoc-lint — Rust documentation linter via `cargo doc`. Runs
# `cargo doc --workspace --no-deps --quiet` with RUSTDOCFLAGS enforcing
# missing-docs and rustdoc::broken_intra_doc_links. Vendored into poly so the
# repo dogfoods `poly hooks` with no external pre-commit dependency.
#
# This is a whole-workspace check: any passed filenames are accepted and
# ignored (the poly hook gates it on `**/*.rs` so it only fires when Rust
# sources change, but `cargo doc` always documents the whole workspace).
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  printf 'rustdoc-lint: skipping — `cargo` not found on PATH.\n' >&2
  exit 0
fi

# Default to --all-features; allow override via RUSTDOC_FEATURES env var.
FEATURES="${RUSTDOC_FEATURES:---all-features}"

export RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links"

exec cargo doc --workspace --no-deps --quiet "$FEATURES"
