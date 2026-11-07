#!/usr/bin/env bash
#MISE description="Measure the core-only poly binary and the marginal size of each engine feature (about 2-3 minutes per build)"
#USAGE arg "[out_dir]" help="Report directory" default="target/size-report"
#USAGE arg "[features]..." help="Cargo features to measure (default: every engine feature)"
# Measure the core-only poly binary and the marginal size of each engine feature on top of it.
#
# Usage: mise run measure:core-size [OUT_DIR [FEATURE...]]
# Builds `poly-cli --no-default-features --features core` once, then once per feature in FEATURES, all with the
# size profile (fat LTO, opt-level "z", one codegen unit, stripped, panic = unwind kept), or with
# SIZE_PROFILE=<name> (for example `core`) that Cargo profile as defined in Cargo.toml. Prints a
# TSV of feature, stripped bytes and delta over the core. Expect roughly 2-3 minutes per build.
set -euo pipefail
shopt -s inherit_errexit

out_dir="${1:-target/size-report}"
mkdir -p "$out_dir"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/size}"
profile="${SIZE_PROFILE:-release}"
if [[ "$profile" == release ]]; then
  export CARGO_PROFILE_RELEASE_LTO=fat
  export CARGO_PROFILE_RELEASE_OPT_LEVEL="${SIZE_OPT_LEVEL:-z}"
  export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
  export CARGO_PROFILE_RELEASE_STRIP=true
fi

FEATURES=("${@:2}")
[[ ${#FEATURES[@]} -gt 0 ]] || FEATURES=(
  poly-core/engine-astgrep poly-core/engine-uncomment poly-core/engine-typos
  poly-core/engine-dockerfile poly-core/engine-dotenv poly-core/engine-ini poly-core/engine-hcl
  poly-core/engine-graphql bundled-mcp
  poly-core/engine-python poly-core/engine-php poly-core/engine-sql
  poly-core/engine-nix
)

build() {
  cargo build --quiet --profile "$profile" --locked -p poly-cli --no-default-features --features core "$@"
  wc -c <"$CARGO_TARGET_DIR/$profile/poly" | tr -d ' '
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
