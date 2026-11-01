"""Permit immutable registry skips only after published payload parity is proven."""

import argparse
import base64
import hashlib
import sys
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from pathlib import Path

from registry_archive import Identity, Payload, compare_payloads, json_object, npm_payload, wheel_payload

Fetch = Callable[[str], bytes]

MAX_DOWNLOAD_BYTES = 256 * 1024 * 1024
NETWORK_TIMEOUT_SECONDS = 60


def fetch_bytes(url: str) -> bytes:
    if urllib.parse.urlsplit(url).scheme != "https":
        raise ValueError("Registry artifact URLs must use HTTPS")
    request = urllib.request.Request(url, headers={"User-Agent": "Poly immutable publication parity"})
    with urllib.request.urlopen(request, timeout=NETWORK_TIMEOUT_SECONDS) as response:
        if urllib.parse.urlsplit(response.geturl()).scheme != "https":
            raise ValueError("Registry redirect must use HTTPS")
        data = response.read(MAX_DOWNLOAD_BYTES + 1)
    if len(data) > MAX_DOWNLOAD_BYTES:
        raise ValueError("Registry response exceeds size limit")
    return data


def registry_metadata(url: str, fetch: Fetch) -> dict[str, object] | None:
    try:
        return json_object(fetch(url))
    except urllib.error.HTTPError as error:
        error.close()
        if error.code == 404:
            return None
        raise


def read_candidate(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_DOWNLOAD_BYTES:
        raise ValueError(f"Missing or oversized candidate archive: {path}")
    return path.read_bytes()


def check_npm(path: Path, fetch: Fetch = fetch_bytes) -> bool:
    identity, candidate = npm_payload(read_candidate(path))
    name, version = identity
    url = (
        "https://registry.npmjs.org/" + urllib.parse.quote(name, safe="@") + "/" + urllib.parse.quote(version, safe="")
    )
    metadata = registry_metadata(url, fetch)
    if metadata is None:
        return False
    if (metadata.get("name"), metadata.get("version")) != identity:
        raise ValueError("npm registry package identity mismatch")
    distribution = metadata.get("dist")
    if not isinstance(distribution, dict) or not isinstance(distribution.get("tarball"), str):
        raise TypeError("Missing npm registry artifact metadata")
    data = fetch(distribution["tarball"])
    integrity = "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()
    if distribution.get("integrity") != integrity:
        raise ValueError("npm registry artifact digest mismatch")
    remote_identity, published = npm_payload(data)
    if remote_identity != identity:
        raise ValueError("Published npm archive identity mismatch")
    compare_payloads(candidate, published)
    return True


def wheel_candidates(paths: list[Path]) -> tuple[Identity, dict[str, Payload]]:
    if not paths:
        raise ValueError("No candidate wheels")
    identity, files = wheel_payload(read_candidate(paths[0]))
    candidates = {paths[0].name: files}
    for path in paths[1:]:
        current, files = wheel_payload(read_candidate(path))
        if path.name in candidates or identity != current:
            raise ValueError("Duplicate filenames or inconsistent candidate wheel identities")
        candidates[path.name] = files
    return identity, candidates


def pypi_files(
    metadata: dict[str, object], identity: Identity, candidates: dict[str, Payload]
) -> dict[str, dict[str, object]]:
    info, urls = metadata.get("info"), metadata.get("urls")
    if not isinstance(info, dict) or not isinstance(urls, list):
        raise TypeError("Malformed PyPI registry artifact inventory")
    if not urls:
        raise ValueError("Missing PyPI registry artifact inventory")
    if (info.get("name"), info.get("version")) != identity:
        raise ValueError("PyPI registry package identity mismatch")
    result = {}
    for row in urls:
        if not isinstance(row, dict) or not isinstance(row.get("filename"), str):
            raise TypeError("Malformed PyPI artifact metadata")
        name = row["filename"]
        if name in result or name not in candidates:
            raise ValueError("Duplicate or unexpected published wheel filename")
        result[name] = row
    return result


def check_wheels(paths: list[Path], fetch: Fetch = fetch_bytes) -> bool:
    identity, candidates = wheel_candidates(paths)
    name, version = identity
    url = (
        "https://pypi.org/pypi/"
        + urllib.parse.quote(name, safe="")
        + "/"
        + urllib.parse.quote(version, safe="")
        + "/json"
    )
    metadata = registry_metadata(url, fetch)
    if metadata is None:
        return False
    published = pypi_files(metadata, identity, candidates)
    for name, row in published.items():
        if not isinstance(row.get("url"), str) or not isinstance(row.get("digests"), dict):
            raise TypeError("Missing PyPI artifact digest or URL")
        data = fetch(row["url"])
        if row["digests"].get("sha256") != hashlib.sha256(data).hexdigest():
            raise ValueError("PyPI registry artifact digest mismatch")
        remote_identity, files = wheel_payload(data)
        if remote_identity != identity:
            raise ValueError("Published wheel identity mismatch")
        compare_payloads(candidates[name], files)
    return len(published) == len(candidates)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=["npm", "wheel"])
    parser.add_argument("archives", type=Path, nargs="+")
    parser.add_argument("--expected-count", type=int)
    args = parser.parse_args()
    expected = args.expected_count if args.expected_count is not None else int(args.kind == "npm")
    if expected <= 0 or len(args.archives) != expected:
        raise ValueError("Candidate archive count mismatch; wheels require a positive --expected-count")
    if args.kind == "npm" and expected != 1:
        raise ValueError("npm verification requires exactly one candidate archive")
    exists = check_npm(args.archives[0]) if args.kind == "npm" else check_wheels(args.archives)
    sys.stdout.write(f"exists={str(exists).lower()}\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, TypeError, OSError) as error:
        sys.stderr.write(f"Publication parity failed: {error}\n")
        raise SystemExit(1) from error
