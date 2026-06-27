#!/bin/bash
# Installer for polylint, polyfmt, and poly binaries
# Usage: curl -fsSL https://raw.githubusercontent.com/Goldziher/polylint/main/install.sh | sh
# Or: ./install.sh [version]

set -eu

# Configuration
REPO="Goldziher/polylint"
VERSION="${1:-latest}"
INSTALL_DIR="${POLY_INSTALL_DIR:-${HOME}/.local/bin}"
TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

# Colors (only if terminal supports them)
if [ -t 1 ]; then
  GREEN='\033[0;32m'
  YELLOW='\033[1;33m'
  RED='\033[0;31m'
  NC='\033[0m' # No Color
else
  GREEN=''
  YELLOW=''
  RED=''
  NC=''
fi

log_info() {
  echo "${GREEN}✓${NC} $1"
}

log_warn() {
  echo "${YELLOW}⚠${NC} $1" >&2
}

log_error() {
  echo "${RED}✗${NC} $1" >&2
}

die() {
  log_error "$1"
  exit 1
}

# Detect OS
if [ "$(uname)" = "Darwin" ]; then
  OS="apple-darwin"
elif [ "$(uname)" = "Linux" ]; then
  OS="linux"
  # Check for musl vs glibc
  if ldd /bin/sh 2>&1 | grep -q musl; then
    OS="${OS}-musl"
  else
    OS="${OS}-gnu"
  fi
else
  die "Unsupported OS: $(uname)"
fi

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
x86_64)
  ARCH="x86_64"
  ;;
aarch64 | arm64)
  ARCH="aarch64"
  ;;
*)
  die "Unsupported architecture: $ARCH"
  ;;
esac

# Determine target triple and file extension
if [ "$OS" = "apple-darwin" ]; then
  TARGET="${ARCH}-apple-darwin"
  EXT="tar.gz"
elif [ "$OS" = "linux-gnu" ] || [ "$OS" = "linux-musl" ]; then
  if [ "$OS" = "linux-musl" ]; then
    TARGET="${ARCH}-unknown-linux-musl"
  else
    TARGET="${ARCH}-unknown-linux-gnu"
  fi
  EXT="tar.gz"
elif [ "$(uname)" = "MINGW64_NT" ] || [ "$(uname)" = "MSYS_NT" ] || [ "$(uname -o)" = "Msys" ]; then
  OS="windows"
  TARGET="x86_64-pc-windows-msvc"
  EXT="zip"
else
  die "Could not determine target triple"
fi

log_info "Detected target: $TARGET"

# Resolve version if "latest" is specified
if [ "$VERSION" = "latest" ]; then
  log_info "Resolving latest release..."
  VERSION_RESPONSE=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest")
  VERSION=$(echo "$VERSION_RESPONSE" | grep -o '"tag_name": "[^"]*"' | cut -d'"' -f4 | sed 's/^v//')
  if [ -z "$VERSION" ]; then
    die "Failed to resolve latest version"
  fi
fi

# Ensure version has v prefix for releases API
VERSION_TAG="v${VERSION#v}"

log_info "Installing version: $VERSION_TAG"

# Construct download URL
ASSET_NAME="poly-${VERSION_TAG#v}-${TARGET}.${EXT}"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION_TAG}/${ASSET_NAME}"

log_info "Downloading from: $DOWNLOAD_URL"

# Download the release asset
if ! curl -fsSL -o "${TEMP_DIR}/${ASSET_NAME}" "$DOWNLOAD_URL"; then
  die "Failed to download $ASSET_NAME"
fi

# Download checksums
if ! curl -fsSL -o "${TEMP_DIR}/sha256sums.txt" \
  "https://github.com/${REPO}/releases/download/${VERSION_TAG}/sha256sums.txt"; then
  log_warn "Could not download checksums; skipping verification"
else
  log_info "Verifying checksum..."
  cd "${TEMP_DIR}"
  # Filter checksum file for our asset
  if grep "${ASSET_NAME}" sha256sums.txt | sha256sum -c - >/dev/null 2>&1; then
    log_info "Checksum verified"
  else
    die "Checksum verification failed for $ASSET_NAME"
  fi
fi

# Extract binaries
log_info "Extracting binaries..."
cd "${TEMP_DIR}"

if [ "$EXT" = "tar.gz" ]; then
  tar xzf "${ASSET_NAME}"
elif [ "$EXT" = "zip" ]; then
  unzip -q "${ASSET_NAME}"
fi

# Create install directory if it doesn't exist
mkdir -p "$INSTALL_DIR"

# Copy binaries to install directory
for binary in poly polylint polyfmt; do
  if [ -f "$binary" ]; then
    cp "$binary" "${INSTALL_DIR}/${binary}"
    chmod +x "${INSTALL_DIR}/${binary}"
    log_info "Installed ${binary} to ${INSTALL_DIR}/${binary}"
  fi
done

# Check if install directory is in PATH
if echo "$PATH" | grep -q "${INSTALL_DIR}"; then
  log_info "Installation complete!"
else
  log_warn "Install directory '${INSTALL_DIR}' is not in your PATH"
  log_warn "Either add it to your PATH or use the full path to run the binaries"
  if [ "$INSTALL_DIR" = "${HOME}/.local/bin" ]; then
    echo "  To add to PATH, run:"
    echo "  export PATH=\"${HOME}/.local/bin:\$PATH\""
    echo "  And add the above to your shell profile (~/.bashrc, ~/.zshrc, etc.)"
  fi
fi
