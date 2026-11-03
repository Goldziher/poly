#!/usr/bin/env bash
# Measure the core-only poly binary and the marginal size of each engine feature on top of it.
#
# Usage: scripts/measure-core-size.sh [OUT_DIR [FEATURE...]]
# Builds `poly-cli --no-default-features` once, then once per feature in FEATURES, all with the
# size profile (fat LTO, opt-level "z", one codegen unit, stripped, panic = unwind kept). Prints a
# TSV of feature, stripped bytes and delta over the core. Expect roughly 2-3 minutes per build.
set -euo pipefail
shopt -s inherit_errexit

out_dir="${1:-target/size-report}"
mkdir -p "$out_dir"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/size}"
export CARGO_PROFILE_RELEASE_LTO=fat
export CARGO_PROFILE_RELEASE_OPT_LEVEL="${SIZE_OPT_LEVEL:-z}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export CARGO_PROFILE_RELEASE_STRIP=true

FEATURES=("${@:2}")
[[ ${#FEATURES[@]} -gt 0 ]] || FEATURES=(
  poly-core/engine-quality poly-core/engine-astgrep poly-core/engine-uncomment poly-core/engine-typos
  poly-core/engine-dockerfile poly-core/engine-dotenv poly-core/engine-ini poly-core/engine-hcl
  poly-core/engine-graphql bundled-mcp
  poly-core/engine-web poly-core/engine-python poly-core/engine-php poly-core/engine-sql
  poly-core/engine-biome poly-core/engine-ruby poly-core/engine-nix
)

build() {
  cargo build --quiet --release --locked -p poly-cli --no-default-features "$@"
  stat -c %s "$CARGO_TARGET_DIR/release/poly"
}

core=$(build)
printf 'feature\tbytes\tdelta\n' | tee "$out_dir/sizes.tsv"
printf 'core\t%s\t0\n' "$core" | tee -a "$out_dir/sizes.tsv"
for feature in "${FEATURES[@]}"; do
  bytes=$(build --features "$feature")
  printf '%s\t%s\t%s\n' "$feature" "$bytes" "$((bytes - core))" | tee -a "$out_dir/sizes.tsv"
done
bytes=$(build --features poly-core/all-engines,bundled-mcp)
printf 'all\t%s\t%s\n' "$bytes" "$((bytes - core))" | tee -a "$out_dir/sizes.tsv"
