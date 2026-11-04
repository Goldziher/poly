#!/usr/bin/env bash
# Release build of the downloadable components, on alef's content-addressed plan:
#
#   components-release.sh previous-lock                  fetch the lock the newest earlier release shipped
#   components-release.sh fetch-alef                     download and checksum-verify the musl alef release
#                                                        binary (for the manylinux container); prints its path
#   components-release.sh plan    <triple>               fingerprint each component's inputs; mark each
#                                                        build or reuse against that lock
#   components-release.sh build   <triple>               compile the rows marked build (thin LTO, stripped)
#   components-release.sh package <triple>               sign and package them, verify
#   components-release.sh lock    <plans> <records> <out>  merge every triple's plan, then write the lock,
#                                                        carrying the reuse rows over verbatim
#
# publish.yaml runs plan, build and package inside a manylinux_2_28 container on linux gnu. Everything
# lands under target/components-release/: previous-lock.json (`{}` when there is none), tools/, plan.json (merged, by `lock`) and, per
# triple, build/ (shared cargo target dir), plan/plan.json and dist/ (archives and *.record.json to upload).
#
# Environment:
#   GH_TOKEN, GITHUB_REPOSITORY     read the previous release (previous-lock)
#   RELEASE_TAG                     tag being released (previous-lock skips it)
#   REBUILD_COMPONENTS              "true" ignores the previous lock, so every component is rebuilt
#   POLY_COMPONENT_SIGNING_KEY      Ed25519 private key, PKCS#8 PEM or base64 (package); never defaulted
#   RUST_TOOLCHAIN                  exact rust version (e.g. 1.99.1) `rustc` must report (plan, build, package)
#   BUILD_IMAGE                     container image the steps run in, folded into the input hash; when set, ALEF
#                                   must be the binary `fetch-alef` verified, since the image has no mise and its
#                                   glibc is too old for the gnu build
#   ALEF                            alef binary to use instead of the version pinned in mise.toml
#
# The plan, --plan and --reuse commands need the alef release that ships them (alef #869); the pin in
# mise.toml selects it, and `fetch-alef` needs the musl binaries of #874.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

KEY_ID="poly-release-1"
LOCK_ASSET="poly-components.lock.json"
# Stripped at link time. A dylib must never be stripped after linking: that removes the exported
# symbols the host looks up.
export RUSTFLAGS="-C strip=symbols"
export CARGO_PROFILE_RELEASE_LTO=thin

# The same inputs must reach plan, build and package: alef refuses to build or package once they
# hash differently from the plan. RUSTFLAGS and CARGO_PROFILE_* are fingerprinted from the environment.
EXTRA=(--extra "key-id=${KEY_ID}" --extra "image=${BUILD_IMAGE:-host}")

STEP="${1:?usage: components-release.sh previous-lock|fetch-alef|plan|build|package|lock ...}"

COMPONENTS_OUT="${ROOT_DIR}/target/components-release"
PREVIOUS_LOCK="${COMPONENTS_OUT}/previous-lock.json"

usage() {
  echo "usage: components-release.sh previous-lock | fetch-alef | plan|build|package <triple> | lock <plans> <records> <out>" >&2
  exit 1
}

pinned_alef_version() {
  local version
  version="$(sed -n 's/^"github:xberg-io\/alef"[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${ROOT_DIR}/mise.toml")"
  [[ -n "${version}" ]] || {
    echo "components-release: no \"github:xberg-io/alef\" pin in mise.toml" >&2
    exit 1
  }
  echo "${version}"
}

# The musl release binary is static, so it runs on the manylinux_2_28 glibc where the gnu one does not.
# It is fetched here, on the host, and checked against the digest GitHub recorded for the release asset;
# the container that runs it also holds the signing key, so an unverified download is not acceptable.
fetch_alef() {
  local version triple asset digest actual dir scratch
  version="$(pinned_alef_version)"
  triple="$(uname -m)-unknown-linux-musl"
  asset="alef-${triple}.tar.gz"
  dir="${COMPONENTS_OUT}/tools/alef-${version}-${triple}"
  digest="$(gh api "repos/xberg-io/alef/releases/tags/v${version}" --jq ".assets[] | select(.name == \"${asset}\") | .digest")"
  [[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo "components-release: no sha256 digest recorded for ${asset} on alef v${version} (got '${digest}')" >&2
    exit 1
  }
  rm -rf "${dir}"
  mkdir -p "${dir}"
  scratch="$(mktemp -d)"
  gh release download "v${version}" --repo xberg-io/alef --pattern "${asset}" --dir "${scratch}"
  actual="sha256:$(sha256_of "${scratch}/${asset}")"
  [[ "${actual}" == "${digest}" ]] || {
    echo "components-release: ${asset} is ${actual}, GitHub records ${digest}" >&2
    exit 1
  }
  tar -xzf "${scratch}/${asset}" -C "${dir}" --strip-components=1
  rm -rf "${scratch}"
  [[ -x "${dir}/alef" ]] || {
    echo "components-release: ${asset} holds no alef binary" >&2
    exit 1
  }
  echo "${dir}/alef"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

alef() {
  if [[ -n "${ALEF:-}" ]]; then
    "${ALEF}" "$@"
  elif [[ -n "${BUILD_IMAGE:-}" ]]; then
    echo "components-release: BUILD_IMAGE is set but ALEF is not; run fetch-alef on the host first" >&2
    exit 1
  else
    mise x -- alef "$@"
  fi
}

check_rustc() {
  : "${RUST_TOOLCHAIN:?${STEP} needs RUST_TOOLCHAIN (an exact rust version)}"
  local actual
  actual="$(rustc --version)"
  [[ "${actual}" == "rustc ${RUST_TOOLCHAIN} "* ]] || {
    echo "components-release: rustc is '${actual}', expected ${RUST_TOOLCHAIN}" >&2
    exit 1
  }
}

# Number of rows the plan marks `build`.
build_rows() {
  jq '[.rows[] | select(.action == "build")] | length' "$1"
}

# The newest other published release that shipped a lock decides: an older one says nothing about
# what the newest shipped. The file is `{}` when there is none, so every component builds. A failing
# gh call (auth, network) is an error, never "no lock".
previous_lock() {
  : "${RELEASE_TAG:?previous-lock needs RELEASE_TAG}"
  : "${GITHUB_REPOSITORY:?previous-lock needs GITHUB_REPOSITORY}"
  mkdir -p "${COMPONENTS_OUT}"
  echo '{}' >"${PREVIOUS_LOCK}"
  if [[ "${REBUILD_COMPONENTS:-false}" == "true" ]]; then
    echo "rebuild requested: ignoring the previous lock"
    return
  fi
  local scratch tags tag assets
  tags="$(gh release list --repo "${GITHUB_REPOSITORY}" --exclude-drafts --exclude-pre-releases --limit 20 \
    --json tagName --jq '.[].tagName')"
  scratch="$(mktemp -d)"
  while read -r tag; do
    [[ -n "${tag}" && "${tag}" != "${RELEASE_TAG}" ]] || continue
    assets="$(gh release view "${tag}" --repo "${GITHUB_REPOSITORY}" --json assets --jq '.assets[].name')"
    if grep -qx "${LOCK_ASSET}" <<<"${assets}"; then
      gh release download "${tag}" --repo "${GITHUB_REPOSITORY}" --pattern "${LOCK_ASSET}" --dir "${scratch}"
      mv "${scratch}/${LOCK_ASSET}" "${PREVIOUS_LOCK}"
      echo "previous lock: ${tag}"
      break
    fi
  done <<<"${tags}"
  rm -rf "${scratch}"
  jq -e 'has("artifacts")' "${PREVIOUS_LOCK}" >/dev/null 2>&1 || [[ "$(cat "${PREVIOUS_LOCK}")" == '{}' ]] || {
    echo "components-release: ${PREVIOUS_LOCK} is neither a component lock nor the empty marker" >&2
    exit 1
  }
  has_previous_lock || echo "no earlier release carries ${LOCK_ASSET}: building every component"
}

has_previous_lock() {
  [[ -f "${PREVIOUS_LOCK}" ]] && jq -e 'has("artifacts")' "${PREVIOUS_LOCK}" >/dev/null 2>&1
}

if [[ "${STEP}" == "previous-lock" ]]; then
  previous_lock
  exit 0
fi

if [[ "${STEP}" == "fetch-alef" ]]; then
  fetch_alef
  exit 0
fi

if [[ "${STEP}" == "lock" ]]; then
  PLANS="${2:?usage: components-release.sh lock <plans> <records> <out>}"
  RECORDS="${3:?usage: components-release.sh lock <plans> <records> <out>}"
  OUTPUT="${4:?usage: components-release.sh lock <plans> <records> <out>}"
  mkdir -p "${COMPONENTS_OUT}" "${RECORDS}"
  MERGED="${COMPONENTS_OUT}/plan.json"
  plan_files=()
  while IFS= read -r file; do plan_files+=("${file}"); done < <(find "${PLANS}" -name plan.json | sort)
  [[ ${#plan_files[@]} -gt 0 ]] || {
    echo "components-release: no plan.json under ${PLANS}" >&2
    exit 1
  }
  jq -s '{schema_version: .[0].schema_version, debug: .[0].debug, rows: (map(.rows) | add)}' "${plan_files[@]}" >"${MERGED}"
  if has_previous_lock; then
    alef component lock --input "${RECORDS}" --output "${OUTPUT}" --reuse "${PREVIOUS_LOCK}" --plan "${MERGED}"
  else
    [[ "$(build_rows "${MERGED}")" == "$(jq '.rows | length' "${MERGED}")" ]] || {
      echo "components-release: the plan reuses components but there is no previous lock" >&2
      exit 1
    }
    alef component lock --input "${RECORDS}" --output "${OUTPUT}"
  fi
  exit 0
fi

TARGET="${2:-}"
[[ -n "${TARGET}" ]] || usage

OUT="${COMPONENTS_OUT}/${TARGET}"
BUILD="${OUT}/build"
DIST="${OUT}/dist"
PLAN="${OUT}/plan/plan.json"

case "${STEP}" in
plan)
  check_rustc
  rm -rf "${OUT}"
  mkdir -p "${DIST}" "$(dirname "${PLAN}")"
  previous=()
  ! has_previous_lock || previous=(--previous-lock "${PREVIOUS_LOCK}")
  alef component plan --target "${TARGET}" ${previous[@]+"${previous[@]}"} "${EXTRA[@]}" --output "${PLAN}"
  rows="$(jq '.rows | length' "${PLAN}")"
  [[ "${rows}" -gt 0 ]] || {
    echo "components-release: alef.toml lists no component for ${TARGET}" >&2
    exit 1
  }
  [[ "$(jq --arg t "${TARGET}" '[.rows[] | select(.target != $t)] | length' "${PLAN}")" == "0" ]] || {
    echo "components-release: the plan has rows for a target other than ${TARGET}" >&2
    exit 1
  }
  jq -r '.rows[] | "\(.component) \(.target) \(.action) \(.input_hash)"' "${PLAN}"
  ;;

build)
  check_rustc
  [[ -f "${PLAN}" ]] || {
    echo "components-release: no plan at ${PLAN}; run the plan step first" >&2
    exit 1
  }
  if [[ "$(build_rows "${PLAN}")" -gt 0 ]]; then
    alef component build --plan "${PLAN}" --target-dir "${BUILD}" "${EXTRA[@]}"
  fi
  ;;

package)
  check_rustc
  [[ -f "${PLAN}" ]] || {
    echo "components-release: no plan at ${PLAN}; run the plan step first" >&2
    exit 1
  }
  expected="$(build_rows "${PLAN}")"
  mkdir -p "${DIST}"
  if [[ "${expected}" -gt 0 ]]; then
    : "${POLY_COMPONENT_SIGNING_KEY:?package needs POLY_COMPONENT_SIGNING_KEY (Ed25519 private key)}"
    alef component package --plan "${PLAN}" --target-dir "${BUILD}" --output "${DIST}" \
      --signing-key-env POLY_COMPONENT_SIGNING_KEY --key-id "${KEY_ID}" "${EXTRA[@]}"
    alef component verify --input "${DIST}"
  fi
  records="$(find "${DIST}" -maxdepth 1 -name '*.record.json' | grep -c . || true)"
  archives="$(find "${DIST}" -maxdepth 1 -name 'poly-component-*.tar.gz' | grep -c . || true)"
  [[ "${records}" == "${expected}" && "${archives}" == "${expected}" ]] || {
    echo "components-release: ${TARGET} plans ${expected} build(s), found ${archives} archive(s) and ${records} record(s)" >&2
    exit 1
  }
  ls -l "${DIST}"
  ;;

*)
  usage
  ;;
esac
