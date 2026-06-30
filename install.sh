#!/bin/sh
# poly installer — downloads the correct prebuilt binaries for this platform.
#
#   curl -fsSL https://raw.githubusercontent.com/Goldziher/polylint/main/install.sh | sh
#
# Installs `poly`, `polylint`, and `polyfmt`. Re-run any time to UPDATE to the
# latest release (it overwrites in place). Pin a version with `POLY_VERSION` or a
# positional argument. POSIX sh — works under dash/ash/bash/zsh.
#
# Environment / flags:
#   POLY_VERSION=v0.1.0       install a specific version (default: latest)
#   POLY_INSTALL_DIR=DIR      install location (default: ~/.local/bin)
#   POLY_NO_MODIFY_PATH=1     do not touch shell profiles
#   --version <v> | <v>       same as POLY_VERSION (positional kept for back-compat)
#   --bin-dir <dir>           same as POLY_INSTALL_DIR
#   --no-modify-path          same as POLY_NO_MODIFY_PATH=1
#   -h | --help               show this help

set -eu

REPO="Goldziher/polylint"
BINARIES="poly polylint polyfmt"

VERSION="${POLY_VERSION:-latest}"
INSTALL_DIR="${POLY_INSTALL_DIR:-${HOME}/.local/bin}"
NO_MODIFY_PATH="${POLY_NO_MODIFY_PATH:-}"

# ---- colors (printf, only on a TTY) ----------------------------------------
if [ -t 1 ]; then
  C_GREEN=$(printf '\033[0;32m')
  C_YELLOW=$(printf '\033[1;33m')
  C_RED=$(printf '\033[0;31m')
  C_DIM=$(printf '\033[2m')
  C_OFF=$(printf '\033[0m')
else
  C_GREEN='' C_YELLOW='' C_RED='' C_DIM='' C_OFF=''
fi

info() { printf '%s✓%s %s\n' "$C_GREEN" "$C_OFF" "$1"; }
warn() { printf '%s⚠%s %s\n' "$C_YELLOW" "$C_OFF" "$1" >&2; }
die() {
  printf '%s✗%s %s\n' "$C_RED" "$C_OFF" "$1" >&2
  exit 1
}

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

# ---- argument parsing (positional version kept for back-compat) ------------
while [ $# -gt 0 ]; do
  case "$1" in
    -h | --help) usage ;;
    --no-modify-path) NO_MODIFY_PATH=1 ;;
    --version)
      shift
      [ $# -gt 0 ] || die "--version needs an argument"
      VERSION="$1"
      ;;
    --version=*) VERSION="${1#*=}" ;;
    --bin-dir)
      shift
      [ $# -gt 0 ] || die "--bin-dir needs an argument"
      INSTALL_DIR="$1"
      ;;
    --bin-dir=*) INSTALL_DIR="${1#*=}" ;;
    -*) die "unknown option: $1 (try --help)" ;;
    *) VERSION="$1" ;;
  esac
  shift
done

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

# ---- detect the target triple ----------------------------------------------
uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_m" in
  x86_64 | amd64) arch="x86_64" ;;
  aarch64 | arm64) arch="aarch64" ;;
  *) die "unsupported architecture: $uname_m" ;;
esac

ext="tar.gz"
case "$uname_s" in
  Darwin) target="${arch}-apple-darwin" ;;
  Linux)
    # musl vs glibc: prefer ldd output, fall back to checking for a musl loader.
    if (ldd --version 2>&1 | grep -qi musl) || [ -e /lib/ld-musl-"${uname_m}".so.1 ]; then
      target="${arch}-unknown-linux-musl"
    else
      target="${arch}-unknown-linux-gnu"
    fi
    ;;
  MINGW* | MSYS* | CYGWIN* | Windows_NT)
    die "Windows detected. Use the PowerShell installer:
  irm https://raw.githubusercontent.com/${REPO}/main/install.ps1 | iex"
    ;;
  *) die "unsupported OS: $uname_s" ;;
esac

info "Platform: ${C_DIM}${target}${C_OFF}"

# ---- resolve the version ----------------------------------------------------
# "latest" is resolved from the /releases/latest redirect (no API token, no rate
# limit); the GitHub API is a fallback for unusual curl builds.
if [ "$VERSION" = "latest" ]; then
  info "Resolving latest release..."
  effective=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
    "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)
  case "$effective" in
    */releases/tag/*) VERSION="${effective##*/tag/}" ;;
    *)
      VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
        grep -o '"tag_name"[^,]*' | head -1 | cut -d'"' -f4)
      ;;
  esac
  [ -n "$VERSION" ] || die "could not resolve the latest release"
fi

tag="v${VERSION#v}"
version="${tag#v}"

asset="poly-${version}-${target}.${ext}"
base="https://github.com/${REPO}/releases/download/${tag}"

# ---- detect an existing install (so we can report install vs update) -------
previous=""
if [ -x "${INSTALL_DIR}/poly" ]; then
  previous=$("${INSTALL_DIR}/poly" --version 2>/dev/null | awk '{print $NF}' || true)
fi

if [ -n "$previous" ] && [ "$previous" = "$version" ]; then
  info "poly ${version} is already installed in ${INSTALL_DIR} — re-installing."
elif [ -n "$previous" ]; then
  info "Updating poly ${previous} → ${version}"
else
  info "Installing poly ${version}"
fi

# ---- download ---------------------------------------------------------------
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

info "Downloading ${C_DIM}${base}/${asset}${C_OFF}"
curl -fsSL -o "${tmp}/${asset}" "${base}/${asset}" ||
  die "failed to download ${asset} (does release ${tag} ship this platform?)"

# ---- verify the checksum (sha256sum, shasum, or openssl) -------------------
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | awk '{print $NF}'
  else
    return 1
  fi
}

if curl -fsSL -o "${tmp}/sha256sums.txt" "${base}/sha256sums.txt" 2>/dev/null; then
  # Lines look like "<sha>  ./poly-<ver>-<triple>.tar.gz"; strip ./ and * markers.
  expected=$(awk -v a="$asset" '{n=$NF; sub(/^[*]/,"",n); sub(/^\.\//,"",n); if (n==a) print $1}' \
    "${tmp}/sha256sums.txt" | head -1)
  actual=$(sha256_of "${tmp}/${asset}" || true)
  if [ -z "$expected" ]; then
    die "no checksum entry for ${asset} — refusing to install unverified binaries"
  elif [ -z "$actual" ]; then
    warn "no sha256 tool (sha256sum/shasum/openssl) found — skipping verification"
  elif [ "$expected" != "$actual" ]; then
    die "checksum mismatch for ${asset} (expected ${expected}, got ${actual})"
  else
    info "Checksum verified"
  fi
else
  die "could not download sha256sums.txt — refusing to install unverified binaries"
fi

# ---- extract & install ------------------------------------------------------
tar xzf "${tmp}/${asset}" -C "$tmp"
mkdir -p "$INSTALL_DIR"
for binary in $BINARIES; do
  [ -f "${tmp}/${binary}" ] || die "expected binary ${binary} missing from ${asset}"
  install -m 0755 "${tmp}/${binary}" "${INSTALL_DIR}/${binary}" 2>/dev/null ||
    { cp "${tmp}/${binary}" "${INSTALL_DIR}/${binary}" && chmod 0755 "${INSTALL_DIR}/${binary}"; }
done
info "Installed poly, polylint, polyfmt → ${INSTALL_DIR}"

# ---- PATH wiring ------------------------------------------------------------
add_path_line='export PATH="'"${INSTALL_DIR}"':$PATH"'
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*)
    : # already on PATH
    ;;
  *)
    if [ -n "$NO_MODIFY_PATH" ]; then
      warn "${INSTALL_DIR} is not on your PATH. Add it with:"
      printf '    %s\n' "$add_path_line"
    else
      modified=""
      for rc in "${HOME}/.profile" "${HOME}/.bashrc" "${HOME}/.zshrc"; do
        [ -e "$rc" ] || continue
        if ! grep -qF "$INSTALL_DIR" "$rc" 2>/dev/null; then
          {
            printf '\n# added by the poly installer\n%s\n' "$add_path_line"
          } >>"$rc"
          modified="${modified} ${rc##*/}"
        fi
      done
      if [ -n "$modified" ]; then
        info "Added ${INSTALL_DIR} to PATH in:${modified}"
        warn "Restart your shell or run: ${add_path_line}"
      else
        warn "${INSTALL_DIR} is not on your PATH. Add it with:"
        printf '    %s\n' "$add_path_line"
      fi
    fi
    ;;
esac

printf '%s\n' "${C_GREEN}poly ${version} is ready.${C_OFF} Run 'poly --help' to get started."
