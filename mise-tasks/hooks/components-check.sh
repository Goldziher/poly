#!/usr/bin/env bash
#MISE description="Whole-workspace clippy, cargo-deny or rustdoc gate for the components/ or components-pyrefly/ cargo workspace"
#USAGE arg "<check>" help="Which gate to run" { choices "clippy" "deny" "rustdoc"; }
#USAGE arg "[workspace]" help="Which workspace (default: components)" default="components" { choices "components" "pyrefly"; }
# Whole-workspace checks for the `components/` and `components-pyrefly/` cargo workspaces, which the root
# workspace excludes so the root `cargo clippy` / `cargo deny` / `cargo doc` never see them.
#
# Usage: mise run hooks:components-check <clippy|deny|rustdoc> [components|pyrefly]
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
  echo "components-check: skipping, cargo not found on PATH." >&2
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
case "${2:-components}" in
components)
  dir="components"
  generated=(poly-engines-web-component poly-engines-biome-component poly-engines-ruby-component)
  ;;
pyrefly)
  dir="components-pyrefly"
  generated=(poly-engines-pyrefly-component)
  ;;
*)
  echo "usage: mise run hooks:components-check <clippy|deny|rustdoc> [components|pyrefly]" >&2
  exit 2
  ;;
esac
manifest="${root}/${dir}/Cargo.toml"
exclude=()
for crate in "${generated[@]}"; do exclude+=(--exclude "$crate"); done

case "${1:-}" in
clippy)
  exec cargo clippy --manifest-path "$manifest" --workspace --all-targets -- -D warnings
  ;;
deny)
  exec cargo deny --manifest-path "$manifest" --config "${root}/${dir}/deny.toml" check
  ;;
rustdoc)
  export RUSTDOCFLAGS="-D missing-docs -D rustdoc::broken_intra_doc_links"
  # alef-generated component producer crates carry no docs (xberg-io/alef generator output).
  exec cargo doc --manifest-path "$manifest" --workspace "${exclude[@]}" --no-deps --quiet --all-features
  ;;
*)
  echo "usage: mise run hooks:components-check <clippy|deny|rustdoc> [components|pyrefly]" >&2
  exit 2
  ;;
esac
