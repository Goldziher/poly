#!/usr/bin/env bash
# Release build of the downloadable components for one target triple, in three steps that
# publish.yaml runs separately (the middle one inside a manylinux_2_28 container on linux gnu).
#
#   components-release.sh plan    <triple>   hash each component's inputs; reuse the previous
#                                            release's asset when the hash is unchanged
#   components-release.sh build   <triple>   cargo build the rest (thin LTO, -C strip=symbols)
#   components-release.sh package <triple>   sign and package them, record the input hashes, verify
#
# Everything lands under target/components-release/<triple>/: build/ (shared cargo target dir),
# dist/ (archives, *.record.json and *.input-hash files to upload) and plan/.
#
# Environment:
#   RELEASE_TAG                     tag being released (plan, package)
#   GH_TOKEN, GITHUB_REPOSITORY     read the previous release (plan)
#   POLY_COMPONENT_SIGNING_KEY      Ed25519 private key, PKCS#8 PEM or base64 (package); never defaulted
#   RUST_TOOLCHAIN                  exact rust version (e.g. 1.99.1) every build uses; folded into the input
#                                   hash and checked against `rustc` before building (plan, build)
#   BUILD_IMAGE                     container image the build step runs in, folded into the input hash
#   REBUILD_COMPONENTS              "true" disables reuse of the previous release's assets (plan)
#   ALEF                            alef binary to use instead of `mise x github:xberg-io/alef`

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT_DIR}"

ALEF_VERSION="0.121.0"
KEY_ID="poly-release-1"
# Stripped at link time. A dylib must never be stripped after linking: that removes the exported
# symbols the host looks up.
COMPONENT_RUSTFLAGS="-C strip=symbols"

STEP="${1:?usage: components-release.sh plan|build|package <triple>}"
TARGET="${2:?usage: components-release.sh plan|build|package <triple>}"

OUT="${ROOT_DIR}/target/components-release/${TARGET}"
BUILD="${OUT}/build"
DIST="${OUT}/dist"
PLAN="${OUT}/plan"
TO_BUILD="${PLAN}/to-build.tsv"

# Windows runners ship `python`, not always `python3`.
if command -v python3 >/dev/null 2>&1; then
  PYTHON=python3
else
  PYTHON=python
fi

check_rustc() {
  : "${RUST_TOOLCHAIN:?${STEP} needs RUST_TOOLCHAIN (an exact rust version)}"
  local actual
  actual="$(rustc --version)"
  [[ "${actual}" == "rustc ${RUST_TOOLCHAIN} "* ]] || {
    echo "components-release: rustc is '${actual}', expected ${RUST_TOOLCHAIN}" >&2
    exit 1
  }
}

alef() {
  if [[ -n "${ALEF:-}" ]]; then
    "${ALEF}" "$@"
  else
    mise x "github:xberg-io/alef@${ALEF_VERSION}" -- alef "$@"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

components() {
  "${PYTHON}" -I "${ROOT_DIR}/scripts/components_release.py" list --target "${TARGET}"
}

input_hash() {
  "${PYTHON}" -I "${ROOT_DIR}/scripts/components_release.py" input-hash --component "$1" --target "${TARGET}" \
    --extra "rust-toolchain=${RUST_TOOLCHAIN}" \
    --extra "key-id=${KEY_ID}" \
    --extra "alef=${ALEF_VERSION}" \
    --extra "rustflags=${COMPONENT_RUSTFLAGS}" \
    --extra "lto=thin" \
    --extra "image=${BUILD_IMAGE:-host}"
}

# Try to fetch the previous release's asset for one component. Only the newest other release that
# carries an input-hash file decides: an older one says nothing about what the newest shipped.
# Returns 1 when the asset cannot be reused.
reuse_previous() {
  local scratch status=0
  scratch="$(mktemp -d)"
  reuse_in "$1" "$2" "${scratch}" || status=$?
  rm -rf "${scratch}"
  return "${status}"
}

reuse_in() {
  [[ "${REBUILD_COMPONENTS:-false}" != "true" ]] || return 1
  local hash="$2" scratch="$3" sidecar="poly-component-$1-${TARGET}.input-hash"
  local tag found="" prev_hash archive source_tag
  while read -r tag; do
    [[ -n "${tag}" && "${tag}" != "${RELEASE_TAG}" ]] || continue
    if gh release download "${tag}" --repo "${GITHUB_REPOSITORY}" --pattern "${sidecar}" --dir "${scratch}" --clobber \
      >/dev/null 2>&1; then
      found="${tag}"
      break
    fi
  done < <(gh release list --repo "${GITHUB_REPOSITORY}" --exclude-drafts --limit 20 --json tagName --jq '.[].tagName')
  [[ -n "${found}" ]] || return 1
  { read -r prev_hash && read -r archive && read -r source_tag; } <"${scratch}/${sidecar}" || return 1
  [[ "${prev_hash}" == "${hash}" ]] || return 1
  gh release download "${source_tag}" --repo "${GITHUB_REPOSITORY}" --pattern "${archive}" \
    --pattern "${archive}.record.json" --dir "${DIST}" --clobber >/dev/null 2>&1 || return 1
  # This only catches a damaged download (archive vs. its own record). It does not authenticate the
  # asset: `alef component verify` in the package step checks the signature against the key in
  # alef.toml, and that is the gate. A key id change alters the input hash, so rotation rebuilds.
  [[ "$(sha256_of "${DIST}/${archive}")" == "$(jq -r '.archive_sha256' "${DIST}/${archive}.record.json")" ]] || {
    rm -f "${DIST}/${archive}" "${DIST}/${archive}.record.json"
    return 1
  }
  printf '%s\n%s\n%s\n' "${hash}" "${archive}" "${source_tag}" >"${DIST}/${sidecar}"
  echo "reused ${archive} from ${source_tag} (input hash ${hash})"
}

case "${STEP}" in
plan)
  : "${RELEASE_TAG:?plan needs RELEASE_TAG}"
  : "${GITHUB_REPOSITORY:?plan needs GITHUB_REPOSITORY}"
  : "${RUST_TOOLCHAIN:?plan needs RUST_TOOLCHAIN (an exact rust version)}"
  # In the container build the host rustc is not the one that builds, so only the pin is hashed;
  # the build step verifies the pin is what it runs.
  [[ -n "${BUILD_IMAGE:-}" ]] || check_rustc
  rm -rf "${OUT}"
  mkdir -p "${DIST}" "${PLAN}"
  : >"${TO_BUILD}"
  count=0
  while IFS=$'\t' read -r component manifest; do
    [[ -n "${component}" ]] || continue
    count=$((count + 1))
    hash="$(input_hash "${component}")"
    printf '%s\n' "${hash}" >"${PLAN}/${component}.hash"
    if reuse_previous "${component}" "${hash}"; then
      continue
    fi
    echo "building ${component} for ${TARGET} (input hash ${hash})"
    printf '%s\t%s\n' "${component}" "${manifest}" >>"${TO_BUILD}"
  done < <(components)
  [[ "${count}" -gt 0 ]] || {
    echo "components-release: alef.toml lists no component for ${TARGET}" >&2
    exit 1
  }
  ;;

build)
  check_rustc
  [[ -f "${TO_BUILD}" ]] || {
    echo "components-release: no plan at ${TO_BUILD}; run the plan step first" >&2
    exit 1
  }
  export CARGO_PROFILE_RELEASE_LTO=thin
  export RUSTFLAGS="${COMPONENT_RUSTFLAGS}"
  # The command `alef component build` runs, without alef: a manylinux container has no
  # alef binary that runs on glibc 2.28, and package reads the library from this target dir.
  while IFS=$'\t' read -r component manifest; do
    [[ -n "${component}" ]] || continue
    cargo build --locked --release --manifest-path "${manifest}" --target "${TARGET}" --target-dir "${BUILD}"
  done <"${TO_BUILD}"
  ;;

package)
  : "${RELEASE_TAG:?package needs RELEASE_TAG}"
  [[ -f "${TO_BUILD}" ]] || {
    echo "components-release: no plan at ${TO_BUILD}; run the plan step first" >&2
    exit 1
  }
  if [[ -s "${TO_BUILD}" ]]; then
    : "${POLY_COMPONENT_SIGNING_KEY:?package needs POLY_COMPONENT_SIGNING_KEY (Ed25519 private key)}"
    while IFS=$'\t' read -r component _manifest; do
      [[ -n "${component}" ]] || continue
      alef component package --component "${component}" --target "${TARGET}" --target-dir "${BUILD}" \
        --output "${DIST}" --signing-key-env POLY_COMPONENT_SIGNING_KEY --key-id "${KEY_ID}"
      record="$(
        for candidate in "${DIST}"/*.record.json; do
          if [[ "$(jq -r '[.manifest.identity.component, .manifest.identity.target] | join(" ")' "${candidate}")" == "${component} ${TARGET}" ]]; then
            echo "${candidate}"
          fi
        done
      )"
      [[ "$(printf '%s\n' "${record}" | grep -c .)" == "1" ]] || {
        echo "components-release: expected one record for ${component} on ${TARGET}, got: ${record}" >&2
        exit 1
      }
      archive="$(jq -r '.archive' "${record}")"
      hash_file="${DIST}/poly-component-${component}-${TARGET}.input-hash"
      printf '%s\n%s\n%s\n' "$(cat "${PLAN}/${component}.hash")" "${archive}" "${RELEASE_TAG}" >"${hash_file}"
    done <"${TO_BUILD}"
  fi
  # Reused records are checked too: signature against the key in alef.toml, archive hash, contract.
  alef component verify --input "${DIST}"
  expected="$(components | grep -c .)"
  records="$(find "${DIST}" -maxdepth 1 -name '*.record.json' | grep -c .)"
  archives="$(find "${DIST}" -maxdepth 1 -name 'poly-component-*.tar.gz' | grep -c .)"
  hashes="$(find "${DIST}" -maxdepth 1 -name '*.input-hash' | grep -c .)"
  [[ "${records}" == "${expected}" && "${archives}" == "${expected}" && "${hashes}" == "${expected}" ]] || {
    echo "components-release: ${TARGET} expected ${expected} component(s), found ${archives} archive(s), ${records} record(s), ${hashes} input hash(es)" >&2
    exit 1
  }
  ls -l "${DIST}"
  ;;

*)
  echo "usage: components-release.sh plan|build|package <triple>" >&2
  exit 1
  ;;
esac
