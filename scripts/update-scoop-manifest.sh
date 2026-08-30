#!/usr/bin/env bash
set -euo pipefail

# update-scoop-manifest.sh <version> <sha256sums-file> <existing-manifest> [release-repo]
#
# Rewrites the Scoop manifest in Goldziher/scoop-bucket (`bucket/poly.json`) for
# a new release and prints it to stdout, the way
# scripts/update-homebrew-formula.sh emits the tap formula.
#
# Only `version`, the 64bit `url`, and the 64bit `hash` move. Everything else —
# `bin` (which carries the poly + polylint shim pair), `checkver`, `autoupdate`,
# `description`, `license` — is carried through from the existing manifest, so a
# field the bucket added by hand cannot be silently dropped by a release. The
# edits are structural (jq) rather than textual for the same reason.
#
# The hash is read from the release's own sha256sums.txt, never recomputed from
# a local file: recomputing is the one way the bucket and the published archive
# can come to disagree.
#
# Scoop publishes no aarch64 Windows archive here because poly does not build
# one; a `arm64` block would point at a file that does not exist.

if [ $# -lt 3 ] || [ $# -gt 4 ]; then
	echo "Usage: $0 <version> <sha256sums-file> <existing-manifest> [release-repo]" >&2
	exit 1
fi

VERSION="$1"
SUMS_FILE="$2"
MANIFEST="$3"
RELEASE_REPO="${4:-Goldziher/poly}"

TARGET="x86_64-pc-windows-msvc"
ASSET="poly-${VERSION}-${TARGET}.zip"
URL="https://github.com/${RELEASE_REPO}/releases/download/v${VERSION}/${ASSET}"

command -v jq >/dev/null 2>&1 || { echo "error: jq is required" >&2; exit 1; }

[ -f "$SUMS_FILE" ] || { echo "error: no such checksums file: $SUMS_FILE" >&2; exit 1; }
[ -f "$MANIFEST" ] || {
	echo "error: no such manifest: $MANIFEST" >&2
	echo "The bucket must already contain bucket/poly.json; this script updates it, it does not seed it." >&2
	exit 1
}

# sha256sums.txt lines look like "<hash>  ./poly-<version>-<triple>.zip" — the
# leading ./ comes from the `find`-based generation in publish.yaml.
HASH="$(awk -v asset="$ASSET" '{
	name = $NF
	sub(/^\*/, "", name)
	sub(/^\.\//, "", name)
	if (name == asset) print $1
}' "$SUMS_FILE" | head -1)"

if [ -z "$HASH" ]; then
	echo "error: ${SUMS_FILE} has no entry for ${ASSET}" >&2
	exit 1
fi

case "$HASH" in
	[0-9a-fA-F]*) [ "${#HASH}" -eq 64 ] || { echo "error: not a sha256: $HASH" >&2; exit 1; } ;;
	*) echo "error: not a sha256: $HASH" >&2; exit 1 ;;
esac

# -a and --indent 4 keep the emitted JSON byte-identical in style to what the
# bucket already holds, so a release diff shows the three fields that moved and
# nothing else.
UPDATED="$(jq -a --indent 4 \
	--arg version "$VERSION" \
	--arg url "$URL" \
	--arg hash "$HASH" \
	'.version = $version
	 | .architecture["64bit"].url = $url
	 | .architecture["64bit"].hash = $hash' "$MANIFEST")"

# Assert the substitutions landed rather than trusting jq's exit status: a
# manifest whose shape drifted (a renamed architecture key, say) would otherwise
# be published still pointing at the previous release — worse than failing.
assert() {
	local what="$1" expected="$2" actual="$3"
	if [ "$expected" != "$actual" ]; then
		echo "error: ${what} did not update (expected '${expected}', got '${actual}')" >&2
		exit 1
	fi
}

assert "version" "$VERSION" "$(printf '%s' "$UPDATED" | jq -r '.version')"
assert "64bit url" "$URL" "$(printf '%s' "$UPDATED" | jq -r '.architecture["64bit"].url')"
assert "64bit hash" "$HASH" "$(printf '%s' "$UPDATED" | jq -r '.architecture["64bit"].hash')"

# `bin` is the poly + polylint shim pair; `checkver`/`autoupdate` let the bucket
# refresh itself if a release ever fails to push. Losing any of them is silent —
# the manifest still installs, it just stops creating the alias or stops
# self-updating — so compare them against the input instead of hoping.
for field in bin checkver autoupdate description license homepage; do
	before="$(jq -cS --arg f "$field" '.[$f] // null' "$MANIFEST")"
	after="$(printf '%s' "$UPDATED" | jq -cS --arg f "$field" '.[$f] // null')"
	assert "$field (must be carried through untouched)" "$before" "$after"
done

if [ "$(printf '%s' "$UPDATED" | jq -r '.bin | if type == "array" then "array" else type end')" = "null" ]; then
	echo "error: the manifest has no 'bin' entry, so scoop would install no command" >&2
	exit 1
fi

printf '%s\n' "$UPDATED"
