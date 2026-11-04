#!/usr/bin/env bash
# Fail when the root workspace carries a git dependency: a `git+` source in Cargo.lock or an
# `allow-git` key in deny.toml. Git-pinned engine crates (oxc, biome, rubyfmt) live in the separate
# `components/` workspace; see adrs/0040.
set -euo pipefail
shopt -s inherit_errexit

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lock="${CHECK_NO_GIT_DEPS_LOCK:-${root}/Cargo.lock}"
deny="${CHECK_NO_GIT_DEPS_DENY:-${root}/deny.toml}"

# An unreadable or empty file would otherwise read as "no git deps".
grep -q '^\[\[package\]\]' "$lock" || {
  echo "check-no-git-deps: $lock lists no packages; refusing to report a pass" >&2
  exit 2
}
grep -q '^\[sources\]' "$deny" || {
  echo "check-no-git-deps: $deny has no [sources] table; refusing to report a pass" >&2
  exit 2
}

status=0
if git_sources="$(grep -n '^source = "git+' "$lock")"; then
  echo "check-no-git-deps: git sources in $lock:" >&2
  echo "$git_sources" >&2
  status=1
fi
if allow_git="$(grep -nE '^[[:space:]]*allow-git[[:space:]]*=' "$deny")"; then
  echo "check-no-git-deps: allow-git in $deny:" >&2
  echo "$allow_git" >&2
  status=1
fi
[[ "$status" -eq 0 ]] && echo "check-no-git-deps: no git sources in Cargo.lock, no allow-git in deny.toml"
exit "$status"
