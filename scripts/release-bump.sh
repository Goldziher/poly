#!/usr/bin/env bash
# Atomically bump the poly version across every shipped surface, then regenerate the
# ai-rulez plugin outputs and assert the plugin manifests carry the new version.
# Usage: ./scripts/release-bump.sh <version>
#   Cargo.toml                            [workspace.package] version
#   npm-package/package.json              version + every optionalDependencies pin
#   npm-package/platforms/*/package.json  version (one per release target)
#   pip-package/pyproject.toml            [project] version
#   pip-package/polylint/__init__.py      __version__
#   .ai-rulez/config.toml                 [plugin] version
#   .claude-plugin/*.json                 generated — asserted to match

set -euo pipefail

VERSION="${1:?usage: release-bump.sh <version>}"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]]; then
	echo "error: version must be MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-rc.N (got '$VERSION')" >&2
	exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

echo "→ Cargo.toml [workspace.package] → $VERSION"
sed -i.bak -E "s/^version = \"[^\"]+\"$/version = \"$VERSION\"/" Cargo.toml
rm Cargo.toml.bak
# Fail loudly if the substitution did not take: a silent no-op (moved key, added
# comment, changed quoting) would otherwise ship a binary reporting the old version.
grep -qxF "version = \"$VERSION\"" Cargo.toml ||
	{ echo "error: Cargo.toml [workspace.package] version bump did not apply" >&2; exit 1; }

# Refresh the workspace lockfile so the tagged build is reproducible. Optional here to keep
# the bump offline-friendly — drop the guard to force it.
if [[ "${SKIP_CARGO_UPDATE:-0}" != "1" ]]; then
	cargo update --workspace >/dev/null 2>&1 || echo "warn: cargo update --workspace skipped/failed (offline?)"
fi

echo "→ npm-package → $VERSION"
# jq rather than sed: the umbrella package carries the version in seven places
# (its own, plus one exact pin per platform package) and a textual substitution
# that matched six of them would publish a package resolving its binary from the
# previous release. Rewrite them structurally, then assert every one below.
npm_tmp="$(mktemp)"
jq --arg v "$VERSION" '.version = $v | .optionalDependencies |= with_entries(.value = $v)' \
	npm-package/package.json > "$npm_tmp"
mv "$npm_tmp" npm-package/package.json

for manifest in npm-package/platforms/*/package.json; do
	npm_tmp="$(mktemp)"
	jq --arg v "$VERSION" '.version = $v' "$manifest" > "$npm_tmp"
	mv "$npm_tmp" "$manifest"
done

echo "→ pip-package → $VERSION"
sed -i.bak -E "s/^version = \"[^\"]+\"$/version = \"$VERSION\"/" pip-package/pyproject.toml
rm pip-package/pyproject.toml.bak
sed -i.bak -E "s/^__version__ = \"[^\"]+\"$/__version__ = \"$VERSION\"/" pip-package/polylint/__init__.py
rm pip-package/polylint/__init__.py.bak

echo "→ .ai-rulez/config.toml [plugin] → $VERSION"
VERSION="$VERSION" perl -0pi -e \
	's/(\[plugin\][^\[]*?\nversion\s*=\s*")[^"]+(")/$1$ENV{VERSION}$2/s' .ai-rulez/config.toml
# Confirm the [plugin] version now reads $VERSION before we regenerate from it.
grep -qxF "version = \"$VERSION\"" .ai-rulez/config.toml ||
	{ echo "error: .ai-rulez/config.toml [plugin] version bump did not apply" >&2; exit 1; }

echo "→ regenerating ai-rulez plugin outputs"
npx -y ai-rulez@latest generate --plugin

# `--plugin` fans out to every harness; poly ships claude + codex plugin surfaces only.
# Prune the out-of-scope bundles (also gitignored) so the tree stays claude/codex-scoped.
rm -rf .cursor-plugin .factory-plugin .hermes .opencode gemini-extension.json \
	kimi.plugin.json package.json .ai-rulez-generated.json

echo
echo "Validating wrapper-package versions..."
validation_failed=0

fail() {
	echo "✗ $1"
	validation_failed=1
}

npm_version="$(jq -r '.version' npm-package/package.json)"
[[ "$npm_version" == "$VERSION" ]] ||
	fail "npm-package/package.json: expected $VERSION, got $npm_version"

# Every pin must move with the package it points at. A pin left behind resolves
# to the previous release's binary under the new version — a fault no test run
# and no `npm install` surfaces, because the install still succeeds.
while read -r pin; do
	[[ "$pin" == "$VERSION" ]] ||
		fail "npm-package/package.json optionalDependencies: expected $VERSION, got $pin"
done < <(jq -r '.optionalDependencies[]' npm-package/package.json)

# The pinned names and the platform directories must describe the same set:
# adding a release target to one and not the other silently drops a platform.
pinned_names="$(jq -r '.optionalDependencies | keys[]' npm-package/package.json | sort)"
platform_names="$(jq -r '.name' npm-package/platforms/*/package.json | sort)"
[[ "$pinned_names" == "$platform_names" ]] ||
	fail "npm-package: optionalDependencies and platforms/ disagree on the package set"

for manifest in npm-package/platforms/*/package.json; do
	platform_version="$(jq -r '.version' "$manifest")"
	[[ "$platform_version" == "$VERSION" ]] ||
		fail "$manifest: expected $VERSION, got $platform_version"
done

grep -qxF "version = \"$VERSION\"" pip-package/pyproject.toml ||
	fail "pip-package/pyproject.toml [project] version bump did not apply"
grep -qxF "__version__ = \"$VERSION\"" pip-package/polylint/__init__.py ||
	fail "pip-package/polylint/__init__.py __version__ bump did not apply"

if [[ $validation_failed -eq 0 ]]; then
	echo "✓ npm + PyPI wrapper packages are consistent: $VERSION"
fi

echo
echo "Validating plugin manifest versions..."

for file in .claude-plugin/plugin.json .claude-plugin/marketplace.json .codex-plugin/plugin.json; do
	if [[ ! -f "$file" ]]; then
		echo "✗ $file: missing (generation did not emit it)"
		validation_failed=1
		continue
	fi
done

if [[ -f .claude-plugin/plugin.json ]]; then
	plugin_version="$(jq -r '.version' .claude-plugin/plugin.json 2>/dev/null || echo '')"
	if [[ "$plugin_version" != "$VERSION" ]]; then
		echo "✗ .claude-plugin/plugin.json: expected $VERSION, got $plugin_version"
		validation_failed=1
	fi
fi

if [[ -f .claude-plugin/marketplace.json ]]; then
	marketplace_version="$(jq -r '.plugins[0].version' .claude-plugin/marketplace.json 2>/dev/null || echo '')"
	if [[ "$marketplace_version" != "$VERSION" ]]; then
		echo "✗ .claude-plugin/marketplace.json: expected $VERSION, got $marketplace_version"
		validation_failed=1
	fi
fi

# The codex surface ships alongside the claude one, so a stale version here is a
# stale release artifact — assert it rather than trusting generation to have run.
if [[ -f .codex-plugin/plugin.json ]]; then
	codex_version="$(jq -r '.version' .codex-plugin/plugin.json 2>/dev/null || echo '')"
	if [[ "$codex_version" != "$VERSION" ]]; then
		echo "✗ .codex-plugin/plugin.json: expected $VERSION, got $codex_version"
		validation_failed=1
	fi
fi

if [[ $validation_failed -eq 0 ]]; then
	echo "✓ Plugin manifests are consistent: $VERSION"
else
	echo "error: version validation failed. Review the above and fix manually." >&2
	exit 1
fi

echo
echo "Done. Review with: git diff"
echo "Next: cargo test --workspace && git commit -am 'chore(release): v$VERSION'"
