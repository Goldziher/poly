#!/usr/bin/env bash
# Publish a 0.0.1 name-reservation placeholder for `poly-lint` to npm.
#
# The unscoped `polylint` name on npm belongs to an unrelated Polymer project, so
# poly's npm package is `poly-lint`. This claims the name at 0.0.1 with a bare,
# install-safe package (NO postinstall, so `npm install poly-lint` cannot fail
# before any release binaries exist). The real download-wrapper ships from
# npm-package/ at the first tagged release via the publish workflow.
#
# Run this once, then configure npm trusted publishing (OIDC) for the repo.
#
# Prerequisite: `npm login` (or an automation token in ~/.npmrc) for an account
# allowed to publish `poly-lint`.
set -euo pipefail

NAME="poly-lint"
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
# poly-lint

Name-reservation placeholder for [poly](https://github.com/Goldziher/polylint), a universal
zero-dependency linter & formatter. This `0.0.1` release only reserves the name; a future release
turns it into a thin installer that downloads the matching prebuilt binary.

The unscoped `polylint` name on npm belongs to an unrelated Polymer project, so poly publishes as
`poly-lint`.
MD

echo "Publishing ${NAME}@${VERSION} as $(npm whoami)..."
( cd "$workdir" && npm publish --access public )

echo "Done. https://www.npmjs.com/package/${NAME}"
