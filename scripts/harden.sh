#!/usr/bin/env bash
# Run poly against real code and report what broke.
#
# Usage:
#   ./scripts/harden.sh                 # corpus B, core tier
#   POLY_HARDEN_CORPUS=a ./scripts/harden.sh
#   POLY_HARDEN_TIER=extended ./scripts/harden.sh
#   ./scripts/harden.sh django ripgrep  # named roots only
#
# One process per root, so a panic in one repository cannot take the rest of the
# run with it, and each root's timing is its own.
#
# Three corpora, and they are allowed to assert different things:
#
#   a  the sibling working trees next to this repo. Unstable and never written
#      to, so their per-rule counts are trend data and never a gate.
#   b  third-party clones pinned to a commit. The only corpus whose counts can
#      gate, because only a pinned input makes a count reproducible.
#   c  machine-generated code. Pinned, but the population is a judgement call,
#      so counts are audit input rather than a gate.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="${POLY_HARDEN_ROOT:-/tmp/poly-harden}"
RESULTS="${POLY_HARDEN_RESULTS:-${WORK}/results.ndjson}"
CORPUS="${POLY_HARDEN_CORPUS:-b}"
TIER="${POLY_HARDEN_TIER:-core}"

# Licences whose trees we are willing to clone and derive a CI artifact from.
# Nothing copyleft: this is not a linking question — nothing is linked — but a
# published artifact derived from a tree is a redistribution one, and the cheap
# answer is to not have the content.
ALLOWED_LICENCES="MIT Apache-2.0 BSD-2-Clause BSD-3-Clause ISC Unlicense MPL-2.0"

mkdir -p "${WORK}"
: >"${RESULTS}"

selected=("$@")
wanted() {
	[ "${#selected[@]}" -eq 0 ] && return 0
	local name="$1" s
	for s in "${selected[@]}"; do [ "${s}" = "${name}" ] && return 0; done
	return 1
}

licence_allowed() {
	local licence="$1" allowed
	for allowed in ${ALLOWED_LICENCES}; do [ "${licence}" = "${allowed}" ] && return 0; done
	return 1
}

# A 40-character hex ref is a commit; anything else is a moving branch, and a
# count measured against one is not reproducible.
is_pinned() { [[ "$1" =~ ^[0-9a-f]{40}$ ]]; }

if [ -z "${POLY_HARDEN_NO_BUILD:-}" ]; then
	echo "==> building the harness"
	cargo test -p poly-cli --test harden --no-run --quiet
fi

declare -a passed=() failed=() unacquired=()

run_root() {
	local name="$1" path="$2"
	echo
	echo "================================================================"
	echo "== ${CORPUS}: ${name}"
	echo "================================================================"
	if POLY_HARDEN_ROOT_PATH="${path}" \
		POLY_HARDEN_ROOT_NAME="${name}" \
		POLY_HARDEN_CORPUS="${CORPUS}" \
		POLY_HARDEN_RESULTS="${RESULTS}" \
		cargo test -p poly-cli --test harden -- --ignored --nocapture --test-threads=1 --exact harden_root; then
		passed+=("${name}")
	else
		failed+=("${name}")
	fi
}

acquire() {
	local name="$1" url="$2" ref="$3" dest="${WORK}/${CORPUS}/${1}"
	# Reuse only a *complete* checkout. A `.git` directory alone proves nothing:
	# a fetch that died half way leaves one behind, and treating that as a hit
	# would measure an empty tree and report it as a pass.
	if [ -d "${dest}/.git" ] && git -C "${dest}" rev-parse --verify -q HEAD >/dev/null 2>&1; then
		echo "==> reusing ${dest}"
		return 0
	fi
	rm -rf "${dest}"
	mkdir -p "${dest}"
	# Depth 1 at an exact ref: poly reads no git history, so depth is pure cost.
	# Retried, because one GitHub hiccup must not look like a poly failure.
	local attempt
	for attempt in 1 2 3; do
		# Each attempt starts from a clean directory. Re-running `git remote add`
		# over an existing `origin` fails, and an `&&` chain would turn that into
		# a permanent failure — the retries would only ever repeat the same error.
		rm -rf "${dest}"
		mkdir -p "${dest}"
		if git init -q "${dest}" 2>/dev/null &&
			git -C "${dest}" remote add origin "${url}" 2>/dev/null &&
			git -C "${dest}" fetch -q --depth=1 origin "${ref}" 2>/dev/null &&
			git -C "${dest}" checkout -q --detach FETCH_HEAD 2>/dev/null; then
			return 0
		fi
		echo "    fetch attempt ${attempt} failed; retrying"
		sleep $((attempt * 5))
	done
	return 1
}

case "${CORPUS}" in
a)
	# Never cloned and never written to: these are live working trees.
	if [ "${CI:-}" = "true" ] && [ -z "${POLY_HARDEN_ALLOW_LOCAL_IN_CI:-}" ]; then
		echo "refusing to run corpus A in CI: these are private local trees" >&2
		exit 2
	fi
	while IFS=$'\t' read -r name path; do
		wanted "${name}" || continue
		run_root "${name}" "${path}"
	done < <("${ROOT_DIR}/scripts/harden/enumerate-local.sh")
	;;
b | c)
	manifest="${ROOT_DIR}/scripts/harden/repos.${CORPUS}.tsv"
	while IFS=$'\t' read -r name url ref licence languages tier; do
		case "${name}" in \#* | "") continue ;; esac
		wanted "${name}" || continue
		# The tier filter is a default, not an override: naming a root on the
		# command line selects it whatever tier it sits in. Applying the tier
		# first meant `harden.sh django` matched nothing and exited 0 having
		# measured nothing, which is the failure this harness exists to catch.
		if [ "${#selected[@]}" -eq 0 ]; then
			[ "${TIER}" = "extended" ] || [ "${tier}" = "core" ] || continue
		fi
		if ! licence_allowed "${licence}"; then
			echo "refusing ${name}: licence ${licence} is not in the allow-list" >&2
			exit 2
		fi
		is_pinned "${ref}" || echo "    note: ${name} is at branch ${ref}, not a commit; its counts are not reproducible"
		if acquire "${name}" "${url}" "${ref}"; then
			run_root "${name}" "${WORK}/${CORPUS}/${name}"
		else
			echo "    could not acquire ${name}"
			unacquired+=("${name}")
		fi
	done <"${manifest}"
	;;
*)
	echo "unknown corpus '${CORPUS}' (expected a, b or c)" >&2
	exit 2
	;;
esac

echo
echo "================================================================"
echo "== summary (corpus ${CORPUS}, tier ${TIER})"
echo "================================================================"
echo "results:     ${RESULTS}"
echo "passed (${#passed[@]}):      ${passed[*]:-<none>}"
echo "failed (${#failed[@]}):      ${failed[*]:-<none>}"
echo "unacquired (${#unacquired[@]}): ${unacquired[*]:-<none>}"

# A run that measured almost nothing is worse than a red one, because it looks
# like success. Fail when acquisition lost more than a fifth of the set.
attempted=$((${#passed[@]} + ${#failed[@]} + ${#unacquired[@]}))
if [ "${attempted}" -gt 0 ] && [ $((${#unacquired[@]} * 5)) -gt "${attempted}" ]; then
	echo "more than 20% of the corpus could not be acquired; this run measured too little to trust" >&2
	exit 1
fi
[ "${#failed[@]}" -eq 0 ]
