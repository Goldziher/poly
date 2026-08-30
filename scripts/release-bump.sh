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
# Scoped to the [plugin] block: a bare `grep -qxF 'version = "X"'` passes on any
# matching line in the file — including the top-level schema `version` — so it
# would report success on a bump that never landed.
plugin_block_version="$(awk '/^\[plugin\]/{inblock=1; next} /^\[/{inblock=0} inblock && /^version[[:space:]]*=/{gsub(/.*= *"|".*/, ""); print; exit}' .ai-rulez/config.toml)"
[[ "$plugin_block_version" == "$VERSION" ]] ||
	{ echo "error: .ai-rulez/config.toml [plugin] version is '$plugin_block_version', expected '$VERSION'" >&2; exit 1; }

echo "→ regenerating ai-rulez plugin outputs"
npx -y ai-rulez@latest generate --plugin

# Every bundle `--plugin` emits is shipped, so nothing is pruned here. The set is
# controlled by `[plugin] runtimes` in .ai-rulez/config.toml, which is the single
# place a harness is added or dropped.

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

# One entry per file `generate --plugin` emits that carries a version, and the
# jq path to it. The set is driven by `[plugin] runtimes` in
# .ai-rulez/config.toml; adding a runtime there means adding its manifest here,
# or the lock-step guarantee quietly stops covering it — a stale manifest still
# installs cleanly, which is the expensive way to find out.
PLUGIN_MANIFESTS=(
	".claude-plugin/plugin.json:.version"
	".claude-plugin/marketplace.json:.plugins[0].version"
	".codex-plugin/plugin.json:.version"
	".cursor-plugin/plugin.json:.version"
	".factory-plugin/plugin.json:.version"
	"gemini-extension.json:.version"
	"kimi.plugin.json:.version"
	"package.json:.version"
)

for entry in "${PLUGIN_MANIFESTS[@]}"; do
	file="${entry%%:*}"
	path="${entry#*:}"
	if [[ ! -f "$file" ]]; then
		echo "✗ $file: missing (generation did not emit it)"
		validation_failed=1
		continue
	fi
	actual="$(jq -r "$path" "$file" 2>/dev/null || echo '')"
	if [[ "$actual" != "$VERSION" ]]; then
		echo "✗ $file ($path): expected $VERSION, got $actual"
		validation_failed=1
	fi
done

# The plugin is worth nothing to a consumer without its skills and slash
# commands, and it shipped without them for several releases: `content_root` was
# set to "." so ai-rulez looked for them at the repository root, where poly has
# none. Assert the payload, not just the version.
for dir in skills commands; do
	if [[ ! -d "$dir" ]] || [[ -z "$(ls -A "$dir" 2>/dev/null)" ]]; then
		echo "✗ $dir/: empty or missing — the plugin would ship no $dir"
		validation_failed=1
	fi
done

skill_count="$(find skills -mindepth 1 -maxdepth 1 -type d 2>/dev/null | wc -l | tr -d ' ')"
command_count="$(find commands -mindepth 1 -maxdepth 1 -name '*.md' 2>/dev/null | wc -l | tr -d ' ')"
if [[ "$skill_count" -lt 1 || "$command_count" -lt 1 ]]; then
	echo "✗ plugin content: $skill_count skills, $command_count commands"
	validation_failed=1
else
	echo "✓ Plugin content: $skill_count skills, $command_count slash commands"
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
