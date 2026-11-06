#!/usr/bin/env bash
#MISE description="Release build of the downloadable components on alef's content-addressed plan (previous-lock, plan, build, package, lock)"
#USAGE arg "<step>" help="Release step" { choices "previous-lock" "fetch-alef" "fetch-mise" "fetch-jq" "plan" "build" "package" "sidecars" "lock"; }
#USAGE arg "[args]..." help="plan, build and package take <triple>; lock takes <plans> <records> <out>; sidecars takes none"
# Release build of the downloadable components, on alef's content-addressed plan:
#
#   mise run components:release previous-lock                  fetch the lock the newest earlier release shipped
#   mise run components:release fetch-alef                     download and checksum-verify the musl alef release
#                                                              binary (for the manylinux container); prints its path
#   mise run components:release fetch-mise                     download and checksum-verify the musl mise release
#                                                              binary (for the manylinux container); prints its path
#   mise run components:release fetch-jq                       download and checksum-verify a static jq (for the container)
#   mise run components:release plan    <triple>               fingerprint each component's inputs; mark each
#                                                              build or reuse against that lock
#   mise run components:release build   <triple>               compile the rows marked build (thin LTO, stripped)
#   mise run components:release package <triple>               sign and package them, verify
#   mise run components:release sidecars                        cross-compile the Go sidecars for every triple from this
#                                                              one host, sign and package them (no plan: alef cannot plan them)
#   mise run components:release lock    <plans> <records> <out>  merge every triple's plan, then write the lock,
#                                                              carrying the reuse rows over verbatim, then add the
#                                                              sidecars' entries
#
# publish.yaml runs plan, build and package inside a manylinux_2_28 container on linux gnu, through the
# musl mise that fetch-mise verified (MISE_TASK_RUN_AUTO_INSTALL=false: the container installs no tools). Everything
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
#                                   must be the binary `fetch-alef` verified, since the image installs no tools
#                                   and its glibc is too old for the gnu build
#   ALEF                            alef binary to use instead of the version pinned in mise.toml
#
# Sidecars are components alef cannot compile (Go executables): alef.toml declares them with
# producer_language = "go", `alef component plan` rejects those, so the plan, build and package steps are
# limited to the Rust components, the `sidecars` step packages the executables with `--library`, and `lock`
# writes their entries separately and merges them. They are rebuilt every release.
#
# The plan, --plan and --reuse commands need the alef release that ships them (alef #869); the pin in
# mise.toml selects it, and `fetch-alef` needs the musl binaries of #874.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

STEP="${1:?usage: mise run components:release previous-lock|fetch-alef|fetch-mise|fetch-jq|plan|build|package|lock ...}"

COMPONENTS_OUT="${ROOT_DIR}/target/components-release"
PREVIOUS_LOCK="${COMPONENTS_OUT}/previous-lock.json"

usage() {
  echo "usage: mise run components:release previous-lock | fetch-alef | fetch-mise | fetch-jq | sidecars | plan|build|package <triple> | lock <plans> <records> <out>" >&2
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

# Download one release asset with gh and check it against the sha256 digest GitHub recorded for it. The
# containers that run these binaries also hold the signing key, so an unverified download is not acceptable.
# Prints the path of the verified file, inside a scratch directory the caller removes.
download_verified() {
  local repo="$1" tag="$2" asset="$3" digest actual scratch
  digest="$(gh api "repos/${repo}/releases/tags/${tag}" --jq ".assets[] | select(.name == \"${asset}\") | .digest")"
  [[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo "components-release: no sha256 digest recorded for ${asset} on ${repo} ${tag} (got '${digest}')" >&2
    exit 1
  }
  scratch="$(mktemp -d)"
  gh release download "${tag}" --repo "${repo}" --pattern "${asset}" --dir "${scratch}"
  actual="sha256:$(sha256_of "${scratch}/${asset}")"
  [[ "${actual}" == "${digest}" ]] || {
    echo "components-release: ${asset} is ${actual}, GitHub records ${digest}" >&2
    exit 1
  }
  echo "${scratch}/${asset}"
}

# The musl release binary is static, so it runs on the manylinux_2_28 glibc where the gnu one does not.
# It is fetched here, on the host, and checked against the digest GitHub recorded for the release asset.
fetch_alef() {
  local version triple asset dir archive
  version="$(pinned_alef_version)"
  triple="$(uname -m)-unknown-linux-musl"
  asset="alef-${triple}.tar.gz"
  dir="${COMPONENTS_OUT}/tools/alef-${version}-${triple}"
  archive="$(download_verified xberg-io/alef "v${version}" "${asset}")"
  rm -rf "${dir}"
  mkdir -p "${dir}"
  tar -xzf "${archive}" -C "${dir}" --strip-components=1
  rm -rf "$(dirname "${archive}")"
  [[ -x "${dir}/alef" ]] || {
    echo "components-release: ${asset} holds no alef binary" >&2
    exit 1
  }
  echo "${dir}/alef"
}

# The same for mise, so the manylinux container can run this very task. The version is the one the host runs,
# which setup-mise pinned; the musl build of it is static, so it runs on the container's old glibc.
fetch_mise() {
  local version arch asset dir archive
  version="$(mise --version | awk '{print $1}')"
  version="${version#v}"
  [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "components-release: cannot read a mise version from 'mise --version' (got '${version}')" >&2
    exit 1
  }
  case "$(uname -m)" in
  x86_64) arch="x64" ;;
  aarch64 | arm64) arch="arm64" ;;
  *)
    echo "components-release: no musl mise release for $(uname -m)" >&2
    exit 1
    ;;
  esac
  asset="mise-v${version}-linux-${arch}-musl.tar.gz"
  dir="${COMPONENTS_OUT}/tools/mise-${version}-linux-${arch}-musl"
  archive="$(download_verified jdx/mise "v${version}" "${asset}")"
  rm -rf "${dir}"
  mkdir -p "${dir}"
  tar -xzf "${archive}" -C "${dir}" --strip-components=2 mise/bin/mise
  rm -rf "$(dirname "${archive}")"
  [[ -x "${dir}/mise" ]] || {
    echo "components-release: ${asset} holds no mise binary" >&2
    exit 1
  }
  echo "${dir}/mise"
}

# jq, which the release steps read plans with and the manylinux image does not ship. jq's release binaries
# are static, so the host fetches and verifies one and the containers (the package one holds the signing key)
# run it from the mount rather than installing a package. Bump the version deliberately.
JQ_VERSION="1.8.2"
fetch_jq() {
  local arch asset dir file
  case "$(uname -m)" in
  x86_64) arch="amd64" ;;
  aarch64 | arm64) arch="arm64" ;;
  *)
    echo "components-release: no static jq release for $(uname -m)" >&2
    exit 1
    ;;
  esac
  asset="jq-linux-${arch}"
  dir="${COMPONENTS_OUT}/tools/jq-${JQ_VERSION}-linux-${arch}"
  file="$(download_verified jqlang/jq "jq-${JQ_VERSION}" "${asset}")"
  rm -rf "${dir}"
  mkdir -p "${dir}"
  mv "${file}" "${dir}/jq"
  rm -rf "$(dirname "${file}")"
  chmod 0755 "${dir}/jq"
  echo "${dir}/jq"
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
    command alef "$@"
  fi
}

# Components alef.toml declares with a Go producer, one name per line.
sidecar_names() {
  python3 - <<'PY'
import tomllib

with open("alef.toml", "rb") as handle:
    config = tomllib.load(handle)
for crate in config["crates"]:
    for component in crate.get("components", []):
        if component.get("producer_language") == "go":
            print(component["name"])
PY
}

# The Rust components, comma-separated: what the plan, build and package steps cover.
rust_components() {
  python3 - <<'PY'
import tomllib

with open("alef.toml", "rb") as handle:
    config = tomllib.load(handle)
names = [
    component["name"]
    for crate in config["crates"]
    for component in crate.get("components", [])
    if component.get("producer_language") != "go"
]
print(",".join(names))
PY
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

if [[ "${STEP}" == "fetch-mise" ]]; then
  fetch_mise
  exit 0
fi

if [[ "${STEP}" == "fetch-jq" ]]; then
  fetch_jq
  exit 0
fi

if [[ "${STEP}" == "lock" ]]; then
  PLANS="${2:?usage: mise run components:release lock <plans> <records> <out>}"
  RECORDS="${3:?usage: mise run components:release lock <plans> <records> <out>}"
  OUTPUT="${4:?usage: mise run components:release lock <plans> <records> <out>}"
  mkdir -p "${COMPONENTS_OUT}" "${RECORDS}"
  MERGED="${COMPONENTS_OUT}/plan.json"
  plan_files=()
  while IFS= read -r file; do plan_files+=("${file}"); done < <(find "${PLANS}" -name plan.json | sort)
  [[ ${#plan_files[@]} -gt 0 ]] || {
    echo "components-release: no plan.json under ${PLANS}" >&2
    exit 1
  }
  jq -s '{schema_version: .[0].schema_version, debug: .[0].debug, rows: (map(.rows) | add)}' "${plan_files[@]}" >"${MERGED}"
  # The sidecars' archives and records are locked on their own: they are in no plan.
  RUST_RECORDS="${COMPONENTS_OUT}/records-rust"
  SIDECAR_RECORDS="${COMPONENTS_OUT}/records-sidecar"
  rm -rf "${RUST_RECORDS}" "${SIDECAR_RECORDS}"
  mkdir -p "${RUST_RECORDS}" "${SIDECAR_RECORDS}"
  shopt -s nullglob
  for file in "${RECORDS}"/*; do
    destination="${RUST_RECORDS}"
    while IFS= read -r name; do
      [[ -n "${name}" && "$(basename "${file}")" == "poly-component-${name}-"* ]] && destination="${SIDECAR_RECORDS}"
    done < <(sidecar_names)
    cp "${file}" "${destination}/"
  done
  RUST_LOCK="${COMPONENTS_OUT}/rust.lock.json"
  if has_previous_lock; then
    alef component lock --input "${RUST_RECORDS}" --output "${RUST_LOCK}" --reuse "${PREVIOUS_LOCK}" --plan "${MERGED}"
  else
    [[ "$(build_rows "${MERGED}")" == "$(jq '.rows | length' "${MERGED}")" ]] || {
      echo "components-release: the plan reuses components but there is no previous lock" >&2
      exit 1
    }
    alef component lock --input "${RUST_RECORDS}" --output "${RUST_LOCK}"
  fi
  if [[ -n "$(sidecar_names)" ]]; then
    sidecar_records="$(find "${SIDECAR_RECORDS}" -maxdepth 1 -name '*.record.json' | grep -c . || true)"
    expected_records="$(($(sidecar_names | grep -c .) * $(python3 -c 'import tomllib; d = tomllib.load(open("alef.toml", "rb")); print(max(len(c.get("targets", [])) for k in d["crates"] for c in k.get("components", []) if c.get("producer_language") == "go"))')))"
    [[ "${sidecar_records}" == "${expected_records}" ]] || {
      echo "components-release: expected ${expected_records} sidecar record(s) under ${RECORDS}, found ${sidecar_records}" >&2
      exit 1
    }
    alef component lock --input "${SIDECAR_RECORDS}" --output "${COMPONENTS_OUT}/sidecar.lock.json"
    jq -s '.[0].artifacts += .[1].artifacts | .[0]' "${RUST_LOCK}" "${COMPONENTS_OUT}/sidecar.lock.json" >"${OUTPUT}"
  else
    cp "${RUST_LOCK}" "${OUTPUT}"
  fi
  exit 0
fi

if [[ "${STEP}" == "sidecars" ]]; then
  : "${POLY_COMPONENT_SIGNING_KEY:?sidecars needs POLY_COMPONENT_SIGNING_KEY (Ed25519 private key)}"
  OUT="${COMPONENTS_OUT}/sidecars"
  DIST="${OUT}/dist"
  rm -rf "${OUT}"
  mkdir -p "${DIST}"
  built=0
  while IFS= read -r name; do
    [[ -n "${name}" ]] || continue
    triples="$(python3 - "${name}" <<'PY'
import sys
import tomllib

with open("alef.toml", "rb") as handle:
    config = tomllib.load(handle)
for crate in config["crates"]:
    for component in crate.get("components", []):
        if component["name"] == sys.argv[1]:
            print(" ".join(component["targets"]))
PY
    )"
    # shellcheck disable=SC2086 # one argument per triple
    mise run sidecar:build "${name}" ${triples} --out "${OUT}/bin" >/dev/null
    for triple in ${triples}; do
      exe="${OUT}/bin/${name}/${triple}/${name}"
      [[ "${triple}" != *windows* ]] || exe="${exe}.exe"
      [[ -f "${exe}" ]] || {
        echo "components-release: no ${name} executable for ${triple} at ${exe}" >&2
        exit 1
      }
      alef component package --component "${name}" --target "${triple}" --library "${exe}" --output "${DIST}" \
        --signing-key-env POLY_COMPONENT_SIGNING_KEY --key-id "${KEY_ID}"
      built=$((built + 1))
    done
  done < <(sidecar_names)
  alef component verify --input "${DIST}"
  records="$(find "${DIST}" -maxdepth 1 -name '*.record.json' | grep -c . || true)"
  archives="$(find "${DIST}" -maxdepth 1 -name 'poly-component-*.tar.gz' | grep -c . || true)"
  [[ "${built}" -gt 0 && "${records}" == "${built}" && "${archives}" == "${built}" ]] || {
    echo "components-release: packaged ${built} sidecar(s), found ${archives} archive(s) and ${records} record(s)" >&2
    exit 1
  }
  ls -l "${DIST}"
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
  alef component plan --component "$(rust_components)" --target "${TARGET}" ${previous[@]+"${previous[@]}"} "${EXTRA[@]}" \
    --output "${PLAN}"
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
