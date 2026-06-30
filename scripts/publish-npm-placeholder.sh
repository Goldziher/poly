#!/usr/bin/env bash
# Publish a 0.0.1 name-reservation placeholder for `@nhirschfeld/polylint` to npm.
#
# npm rejects the unscoped names `polylint` (taken by a Polymer project) and
# `poly-lint` (too similar to it), so poly's npm package is the scoped
# `@nhirschfeld/polylint`. This claims it at 0.0.1 with a bare, install-safe
# package (NO postinstall, so installing it cannot fail before any release
# binaries exist). The real download-wrapper ships from npm-package/ at the first
# tagged release via the publish workflow.
#
# Run this once, then configure npm trusted publishing (OIDC) for the repo.
#
# Prerequisite: `npm login` for an account allowed to publish under @nhirschfeld.
set -euo pipefail

NAME="@nhirschfeld/polylint"
VERSION="0.0.1"

if ! npm whoami >/dev/null 2>&1; then
  echo "error: not logged in to npm. Run 'npm login' first." >&2
  exit 1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

cat > "$workdir/package.json" <<JSON
{
  "name": "${NAME}",
  "version": "${VERSION}",
  "description": "Universal zero-dependency linter & formatter (poly) — name reservation placeholder",
  "license": "(MIT OR Apache-2.0)",
  "homepage": "https://github.com/Goldziher/polylint#readme",
  "repository": { "type": "git", "url": "git+https://github.com/Goldziher/polylint.git" },
  "bugs": { "url": "https://github.com/Goldziher/polylint/issues" },
  "author": "Na'aman Hirschfeld <nhirschfeld@gmail.com>",
  "keywords": ["linter", "formatter", "lint", "format", "polyglot"],
  "files": ["README.md"]
}
JSON

cat > "$workdir/README.md" <<'MD'
# @nhirschfeld/polylint

Name-reservation placeholder for [poly](https://github.com/Goldziher/polylint), a universal
zero-dependency linter & formatter. This `0.0.1` release only reserves the name; a future release
turns it into a thin installer that downloads the matching prebuilt binary.

npm rejects the unscoped names `polylint` (a Polymer project) and `poly-lint` (too similar), so
poly publishes under the `@nhirschfeld` scope.
MD

echo "Publishing ${NAME}@${VERSION} as $(npm whoami)..."
( cd "$workdir" && npm publish --access public )

echo "Done. https://www.npmjs.com/package/${NAME}"
