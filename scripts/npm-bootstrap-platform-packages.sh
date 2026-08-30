#!/usr/bin/env bash
# One-time bootstrap for the SIX npm platform packages.
#
# The umbrella package @goldziher/polylint carries no binary. It pins one
# platform package per target through optionalDependencies, and npm installs
# only the one matching the host's os/cpu/libc. Those six packages do not exist
# on the registry yet, and npm will not let a trusted publisher be configured
# for a package that has never been published — the same chicken-and-egg the
# umbrella already went through, six more times.
#
# This publishes a stub of each so you can configure their trusted publishers.
# Each stub carries the real os/cpu/libc constraints, so it can never install
# anywhere unexpected, and no binary.
#
# Usage:
#   ./scripts/npm-bootstrap-platform-packages.sh              # dry run
#   ./scripts/npm-bootstrap-platform-packages.sh --publish
#   ./scripts/npm-bootstrap-platform-packages.sh --publish --otp 123456
#
# Already-published packages are skipped, so re-running after a partial failure
# only does what is left.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="0.0.0"

PUBLISH=0
OTP=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--publish) PUBLISH=1 ;;
	--otp)
		OTP="${2:?--otp needs a code}"
		shift
		;;
	-h | --help)
		sed -n '2,22p' "$0" | sed 's/^# \?//'
		exit 0
		;;
	*)
		echo "error: unknown argument '$1' (try --help)" >&2
		exit 2
		;;
	esac
	shift
done

say() { printf '\n\033[1m%s\033[0m\n' "$*"; }
fail() {
	printf '\033[31merror:\033[0m %s\n' "$*" >&2
	exit 1
}

say "Preflight"
command -v npm >/dev/null 2>&1 || fail "npm is not installed"
command -v jq >/dev/null 2>&1 || fail "jq is not installed"
WHO="$(npm whoami 2>/dev/null || true)"
[[ -n "$WHO" ]] || fail "not logged in to npm. Run: npm login"
echo "  authenticated as    $WHO"

MANIFESTS=(npm-package/platforms/*/package.json)
[[ -e "${MANIFESTS[0]}" ]] || fail "no platform manifests under npm-package/platforms/"
echo "  platform manifests  ${#MANIFESTS[@]}"

# Names, os/cpu/libc and the umbrella's pin set all come from the real manifests
# rather than being restated here — a list duplicated in a bootstrap script is a
# list that goes stale the first time a target is added.
say "Packages"
TODO=()
for manifest in "${MANIFESTS[@]}"; do
	NAME="$(jq -r .name "$manifest")"
	# `npm view` on a missing package exits non-zero AND prints an E404 object on
	# stdout, so the exit status is the only trustworthy signal here.
	if npm view "$NAME" versions --json >/dev/null 2>&1; then
		printf '  %-38s already published — skipping\n' "$NAME"
	else
		printf '  %-38s needs bootstrap\n' "$NAME"
		TODO+=("$manifest")
	fi
done

if [[ ${#TODO[@]} -eq 0 ]]; then
	say "Nothing to do"
	echo "  All six platform packages exist. Configure any missing trusted publishers,"
	echo "  then cut a release."
	exit 0
fi

publish_one() {
	local manifest="$1" staging name
	name="$(jq -r .name "$manifest")"
	staging="$(mktemp -d)"

	# Carry os/cpu/libc through verbatim so a stub can never be installed on a
	# platform it does not describe; drop everything else, including any bin.
	jq --arg v "$VERSION" '{
		name: .name,
		version: $v,
		description: "Placeholder. The real platform package is published by CI from the poly repository.",
		license: "MIT",
		homepage: "https://github.com/Goldziher/poly#readme",
		repository: { type: "git", url: "git+https://github.com/Goldziher/poly.git" },
		os: .os, cpu: .cpu
	} + (if .libc then { libc: .libc } else {} end)' "$manifest" >"$staging/package.json"

	printf '# %s — placeholder\n\nNo binary. Published so that npm trusted publishing could be configured for this package.\nInstall poly from <https://github.com/Goldziher/poly>.\n' "$name" >"$staging/README.md"
	cp LICENSE "$staging/LICENSE"

	local args=(publish --access public --tag placeholder)
	[[ "$PUBLISH" -eq 1 ]] || args+=(--dry-run)
	[[ -n "$OTP" ]] && args+=(--otp "$OTP")
	# npm writes its whole tarball-contents notice to stderr, six times over, which
	# buries the one line that matters. Captured and replayed only on failure —
	# and captured via a variable rather than a pipeline, because a pipeline
	# reports the filter's status and a rejected publish would read as success.
	local output status=0
	output="$(cd "$staging" && npm "${args[@]}" 2>&1)" || status=$?
	rm -rf "$staging"
	if [[ $status -ne 0 ]]; then
		printf '  %-38s FAILED\n' "$name"
		printf '%s\n' "$output" | sed 's/^/      /'
		return "$status"
	fi
	printf '  %-38s %s\n' "$name" "$([[ "$PUBLISH" -eq 1 ]] && echo published || echo 'ok (dry run)')"
}

if [[ "$PUBLISH" -ne 1 ]]; then
	say "Dry run"
	for manifest in "${TODO[@]}"; do publish_one "$manifest"; done
	say "Nothing was published"
	echo "  Re-run with --publish to do it for real:"
	echo "    $0 --publish"
	exit 0
fi

say "Confirm"
echo "  This publishes ${#TODO[@]} package(s) at $VERSION to the public npm registry."
echo "  npm severely restricts unpublishing, so treat this as permanent."
printf '  Type yes to continue: '
read -r REPLY_TEXT
[[ "$REPLY_TEXT" == "yes" ]] || fail "not confirmed; nothing was published"

say "Publishing"
for manifest in "${TODO[@]}"; do publish_one "$manifest"; done

say "Now configure a trusted publisher for each"
echo "  For every package below: npmjs.com → the package → Settings → trusted publisher"
echo "      repository     Goldziher/poly"
echo "      workflow file  publish.yaml"
echo "      environment    (leave empty)"
echo
for manifest in "${MANIFESTS[@]}"; do
	echo "    https://www.npmjs.com/package/$(jq -r .name "$manifest")/access"
done
echo
echo "  Until every one is configured, the npm publish job fails on the first"
echo "  package without a publisher and the umbrella is never published — which is"
echo "  the safe order: an umbrella whose pins do not exist installs cleanly and"
echo "  gives the user no poly binary at all."
