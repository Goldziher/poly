#!/usr/bin/env bash
# Stage the prebuilt release binaries into the six npm platform packages.
#
#   ./stage-binaries.sh --artifacts <dir>   # the release archives, as published
#   ./stage-binaries.sh --local <binary>    # one locally built poly, host platform only
#
# The archives are the *published* ones — the same files `checksums` hashed into
# sha256sums.txt and the same files attached to the GitHub release. Re-building a
# binary here instead would silently ship bytes the release checksums do not cover.
set -euo pipefail

PACKAGE_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PACKAGE_DIR"

VERSION="$(node -p 'require("./package.json").version')"

# platform-package-name:release-triple
TARGETS=(
	"darwin-arm64:aarch64-apple-darwin"
	"darwin-x64:x86_64-apple-darwin"
	"linux-arm64-gnu:aarch64-unknown-linux-gnu"
	"linux-x64-gnu:x86_64-unknown-linux-gnu"
	"linux-x64-musl:x86_64-unknown-linux-musl"
	"win32-x64:x86_64-pc-windows-msvc"
)

usage() {
	echo "usage: $0 --artifacts <dir> | --local <path-to-poly>" >&2
	exit 2
}

MODE=""
ARG=""
case "${1:-}" in
	--artifacts | --local)
		MODE="$1"
		ARG="${2:?$(usage)}"
		;;
	*) usage ;;
esac

stage_from_archive() {
	local name="$1" triple="$2" archive_dir="$3"
	local dest="$PACKAGE_DIR/platforms/$name/bin"
	rm -rf "$dest"
	mkdir -p "$dest"

	if [[ "$triple" == *windows* ]]; then
		local archive="$archive_dir/poly-$VERSION-$triple.zip"
		[[ -f "$archive" ]] || { echo "error: missing archive $archive" >&2; return 1; }
		unzip -q -o "$archive" poly.exe -d "$dest"
		[[ -f "$dest/poly.exe" ]] || { echo "error: $archive contained no poly.exe" >&2; return 1; }
	else
		local archive="$archive_dir/poly-$VERSION-$triple.tar.gz"
		[[ -f "$archive" ]] || { echo "error: missing archive $archive" >&2; return 1; }
		tar xzf "$archive" -C "$dest" poly
		[[ -x "$dest/poly" ]] || { echo "error: $archive contained no executable poly" >&2; return 1; }
	fi
	echo "  ✓ $name ← poly-$VERSION-$triple"
}

if [[ "$MODE" == "--artifacts" ]]; then
	[[ -d "$ARG" ]] || { echo "error: not a directory: $ARG" >&2; exit 1; }
	echo "Staging poly $VERSION binaries from $ARG"
	for target in "${TARGETS[@]}"; do
		stage_from_archive "${target%%:*}" "${target##*:}" "$(cd "$ARG" && pwd)"
	done
	exit 0
fi

# --local: one binary, for testing the packaging end to end without a release.
[[ -x "$ARG" ]] || { echo "error: not an executable: $ARG" >&2; exit 1; }
case "$(uname -s)/$(uname -m)" in
	Darwin/arm64) name="darwin-arm64" ;;
	Darwin/x86_64) name="darwin-x64" ;;
	Linux/aarch64) name="linux-arm64-gnu" ;;
	Linux/x86_64) name="linux-x64-gnu" ;;
	*) echo "error: --local has no platform package for $(uname -s)/$(uname -m)" >&2; exit 1 ;;
esac
dest="$PACKAGE_DIR/platforms/$name/bin"
rm -rf "$dest"
mkdir -p "$dest"
cp "$ARG" "$dest/poly"
chmod +x "$dest/poly"
echo "  ✓ $name ← $ARG (local build; host platform only)"
