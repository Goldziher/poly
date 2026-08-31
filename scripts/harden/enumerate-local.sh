#!/usr/bin/env bash
# Enumerate corpus A: the sibling working trees next to this repository.
#
# Emits one TSV row per root: name<TAB>absolute path. The caller records the
# result verbatim, because "what did you actually measure" is the first question
# anyone asks of a corpus number and a list is the only honest answer.
#
# Four traps, each of which has already produced a wrong measurement:
#
#   * A plain `ls` misses nested roots. `xberg-io` is a polyrepo whose own
#     .gitignore hides the projects inside it, so scanning it as one root sees
#     almost nothing.
#   * `find -type d -name .git` misses repositories. A worktree's `.git` is a
#     *file*, and at least one sibling is not a git repository at all.
#   * `git ls-files` must never be used to enumerate files. It exits 128 outside
#     a repository and, with stderr swallowed, returns an empty list — which
#     reads as "no files" rather than "not a repository". poly itself enumerates
#     the files; this script only finds roots.
#   * Worktree containers duplicate a project many times over. Roots sharing a
#     common git directory are collapsed to one.

set -euo pipefail

WORKSPACE="${POLY_HARDEN_LOCAL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"

# A directory is a root when it carries a project marker. Checked with -e rather
# than -f so a `.git` file (worktree) counts alongside a `.git` directory.
is_root() {
	local dir="$1"
	for marker in .git Cargo.toml package.json pyproject.toml go.mod composer.json Taskfile.yml; do
		[ -e "${dir}/${marker}" ] && return 0
	done
	return 1
}

# Collapse worktrees: two roots sharing a git common directory are one project.
common_dir() {
	git -C "$1" rev-parse --git-common-dir 2>/dev/null | while read -r d; do
		(cd "$1" && cd "$(dirname "$d")" 2>/dev/null && pwd -P) || echo "$1"
	done
}

seen_file="$(mktemp)"
trap 'rm -f "${seen_file}"' EXIT

emit() {
	local name="$1" path="$2" key
	key="$(common_dir "${path}")"
	[ -z "${key}" ] && key="${path}"
	if grep -qxF "${key}" "${seen_file}" 2>/dev/null; then return 0; fi
	echo "${key}" >>"${seen_file}"
	printf '%s\t%s\n' "${name}" "${path}"
}

for dir in "${WORKSPACE}"/*/; do
	dir="${dir%/}"
	name="$(basename "${dir}")"
	case "${name}" in *-worktrees | .worktrees | worktrees) continue ;; esac
	is_root "${dir}" || continue
	emit "${name}" "${dir}"

	# One level down, for polyrepos whose nested projects their own ignore rules
	# hide from a top-level walk.
	for nested in "${dir}"/*/; do
		nested="${nested%/}"
		nested_name="$(basename "${nested}")"
		case "${nested_name}" in *-worktrees | .worktrees | worktrees | node_modules | target) continue ;; esac
		is_root "${nested}" || continue
		emit "${name}/${nested_name}" "${nested}"
	done
done
