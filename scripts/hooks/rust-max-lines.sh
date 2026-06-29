#!/usr/bin/env bash
# rust-max-lines — fail when any passed Rust source exceeds the configured
# line-count ceiling. Counts total file lines (including blanks and comments)
# via `wc -l`, which is fast and parser-free. Vendored into poly so the repo
# dogfoods `poly hooks` with no external pre-commit dependency.
#
# Test files are excluded by default: a path under a `tests/` directory at any
# depth, or a file named `tests.rs`. Pass `--include-tests` to count them.
#
#   args: ["--max=<N>"]        default 1000
#   args: ["--include-tests"]  also count files under tests/ and *.rs named tests.rs
set -euo pipefail

MAX_LINES=1000
INCLUDE_TESTS=0
files=()

for arg in "$@"; do
  case "$arg" in
  --max=*)
    MAX_LINES="${arg#--max=}"
    ;;
  --max)
    printf 'rust-max-lines: --max requires "=N" form (e.g. --max=1000)\n' >&2
    exit 2
    ;;
  --include-tests)
    INCLUDE_TESTS=1
    ;;
  --*)
    printf 'rust-max-lines: unknown flag %q\n' "$arg" >&2
    exit 2
    ;;
  *)
    files+=("$arg")
    ;;
  esac
done

if ! [[ "$MAX_LINES" =~ ^[1-9][0-9]*$ ]]; then
  printf 'rust-max-lines: --max must be a positive integer, got %q\n' "$MAX_LINES" >&2
  exit 2
fi

if [[ ${#files[@]} -eq 0 ]]; then
  exit 0
fi

is_test_file() {
  local path="$1"
  path="${path#./}"
  if [[ "$path" == tests/* || "$path" == */tests/* ]]; then
    return 0
  fi
  if [[ "$path" == tests.rs || "$path" == */tests.rs ]]; then
    return 0
  fi
  return 1
}

status=0
for file in "${files[@]}"; do
  if [[ ! -f "$file" ]]; then
    continue
  fi
  if ((INCLUDE_TESTS == 0)) && is_test_file "$file"; then
    continue
  fi
  lines=$(wc -l <"$file" | tr -d '[:space:]')
  if ((lines > MAX_LINES)); then
    printf '%s: %d lines exceeds limit of %d\n' "$file" "$lines" "$MAX_LINES" >&2
    status=1
  fi
done

exit "$status"
