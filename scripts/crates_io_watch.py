"""Fail when a crate that blocks poly's crates.io-only core gets a real release.

Exit codes: 0 all still blocked, 1 a blocked crate has a real release, 2 crates.io could not be queried.
"""

# poly: allow-file[T201] a CI watch script reports on stdout as GitHub workflow commands; there is no logger.

import json
import re
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Callable

API = "https://crates.io/api/v1/crates/{}"
USER_AGENT = "poly-crates-io-watch (+https://github.com/Goldziher/poly)"
ATTEMPTS = 3
RETRY_DELAY_SECONDS = 2

# crate -> stale baseline version. A release counts only when strictly greater than the baseline;
# None means any real (non-placeholder) release counts. 0.0.0* versions are always placeholders.
BLOCKED: dict[str, str | None] = {
    "oxc_linter": None,
    "oxc_formatter": None,
    "oxc_formatter_core": None,
    "oxc_formatter_css": None,
    "oxc_formatter_json": None,
    "oxc_formatter_markdown": None,
    "oxc_formatter_toml": None,
    "oxc_formatter_yaml": None,
    "biome_analyze": "0.5.7",
    "biome_css_analyze": None,
    "biome_css_parser": "0.5.8",
    "biome_graphql_analyze": None,
    "biome_graphql_parser": None,
    "biome_languages": None,
    "rubyfmt": None,
}

_VERSION = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$")


class Unreachable(Exception):
    """crates.io could not be queried (network, HTTP or JSON failure)."""


def parse_version(text: str) -> tuple[tuple[int, int, int], str | None] | None:
    """Split a version into its numeric triple and prerelease tag; None if it is not semver."""
    match = _VERSION.match(text)
    if match is None:
        return None
    return (int(match[1]), int(match[2]), int(match[3])), match[4]


def fetch_versions(name: str, sleep: Callable[[float], None] | None = None) -> list[tuple[str, bool]] | None:
    """Return (version, yanked) pairs for a crate, or None when crates.io has no such crate."""
    request = urllib.request.Request(API.format(name), headers={"User-Agent": USER_AGENT})
    reason = ""
    for attempt in range(ATTEMPTS):
        if attempt:
            (sleep or time.sleep)(RETRY_DELAY_SECONDS)
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                payload = json.load(response)
            return [(entry["num"], bool(entry.get("yanked"))) for entry in payload["versions"]]
        except urllib.error.HTTPError as error:
            if error.code == 404:
                return None
            reason = f"HTTP {error.code}"
        except (urllib.error.URLError, TimeoutError, OSError, ValueError, KeyError, TypeError) as error:
            reason = f"{type(error).__name__}: {error}"
    raise Unreachable(f"{name}: {reason}")


def real_releases(versions: list[tuple[str, bool]], baseline: str | None) -> tuple[list[str], list[str]]:
    """Split a crate's live versions into (real releases, prereleases worth a notice)."""
    floor = parse_version(baseline)[0] if baseline else None
    real: list[str] = []
    pre: list[str] = []
    for number, yanked in versions:
        parsed = parse_version(number)
        if yanked or parsed is None:
            continue
        triple, prerelease = parsed
        if triple == (0, 0, 0):
            continue
        if floor is not None and triple <= floor:
            continue
        (pre if prerelease else real).append(number)
    return real, pre


def check(
    blocked: dict[str, str | None], fetch: Callable[[str], list[tuple[str, bool]] | None] = fetch_versions
) -> tuple[list[str], list[str]]:
    """Return (errors, notices). Raises Unreachable if any crate cannot be queried."""
    errors: list[str] = []
    notices: list[str] = []
    for name, baseline in blocked.items():
        versions = fetch(name)
        if versions is None:
            continue
        real, pre = real_releases(versions, baseline)
        if real:
            errors.append(f"{name} {real[0]} is published on crates.io; switch its bridge crate off the git pin")
        elif pre:
            notices.append(f"{name} has prerelease {pre[0]} on crates.io; not counted as a release")
    return errors, notices


def main() -> int:
    """Run the watch and map the outcome to an exit code."""
    try:
        errors, notices = check(BLOCKED)
    except Unreachable as error:
        print(f"::error::crates.io unreachable for {error}")
        return 2
    for message in notices:
        print(f"::notice::{message}")
    for message in errors:
        print(f"::error::{message}")
    if not errors:
        print(f"all {len(BLOCKED)} watched crates are still blocked")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
