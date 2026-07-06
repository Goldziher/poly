from __future__ import annotations

import hashlib
import os
import platform
import shutil
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from pathlib import Path
from urllib.error import URLError
from urllib.request import Request, urlopen

import certifi

# The binary shipped in the release archive.
BINARIES = ("poly",)


def _is_apple_silicon(machine: str) -> bool:
    """Detect Apple Silicon hardware, even from an x86_64 process under Rosetta.

    ``platform.machine()`` reflects the *process* arch, so an x86_64 Python under
    Rosetta reports ``x86_64`` on Apple Silicon hardware. Probe a hardware-level
    signal the translation layer cannot spoof: ``sysctl -n hw.optional.arm64`` is
    ``1`` on Apple Silicon.
    """
    if machine in {"aarch64", "arm64"}:
        return True
    try:
        result = subprocess.run(
            ["sysctl", "-n", "hw.optional.arm64"],
            capture_output=True,
            text=True,
            check=False,
        )
    except (OSError, ValueError):
        return False
    return result.stdout.strip() == "1"


def _platform_triple() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()

    if system == "windows":
        if machine in {"amd64", "x86_64"}:
            return "x86_64-pc-windows-msvc"
        if machine in {"x86", "i386", "i686"}:
            raise RuntimeError("32-bit Windows is not supported")
    elif system == "linux":
        if machine in {"amd64", "x86_64"}:
            return "x86_64-unknown-linux-gnu"
        if machine in {"aarch64", "arm64"}:
            return "aarch64-unknown-linux-gnu"
    elif system == "darwin":
        if _is_apple_silicon(machine):
            return "aarch64-apple-darwin"
        if machine in {"amd64", "x86_64"}:
            return "x86_64-apple-darwin"

    raise RuntimeError(f"Unsupported platform: {system} {machine}")


def _asset(version: str) -> tuple[str, str, str, str]:
    """Return (archive_url, ext, asset_name, checksums_url) for this platform.

    poly release assets embed the package version (without a leading ``v``) in the
    archive name, e.g. ``poly-0.1.0-x86_64-apple-darwin.tar.gz``.
    """
    triple = _platform_triple()
    ext = "zip" if "windows" in triple else "tar.gz"
    asset_name = f"poly-{version}-{triple}.{ext}"
    base = f"https://github.com/Goldziher/poly/releases/download/v{version}"
    archive_url = f"{base}/{asset_name}"
    checksums_url = f"{base}/sha256sums.txt"
    return archive_url, ext, asset_name, checksums_url


def _is_retryable_error(error: Exception | str) -> bool:
    """Check if an error is transient and worth retrying."""
    error_str = str(error).lower()
    # Retry on network timeouts, connection errors, and HTTP 5xx
    return any(
        substring in error_str
        for substring in [
            "timeout",
            "connection",
            "refused",
            "reset",
            "unreachable",
            "http 5",
            "temporarily unavailable",
        ]
    )


def _retry_with_backoff(fn, max_attempts: int = 3, delays: list[int] | None = None) -> None:
    """Execute fn with exponential backoff retry on transient errors.

    Only retries on transient errors (network, 5xx). Deterministic failures
    (404, bad checksum) propagate immediately.
    """
    if delays is None:
        delays = [1, 2, 4]  # exponential: 1s, 2s, 4s

    last_error = None
    for attempt in range(max_attempts):
        try:
            return fn()
        except Exception as error:
            last_error = error
            if not _is_retryable_error(error) or attempt >= max_attempts - 1:
                raise

            delay = delays[attempt]
            print(
                f"Transient error (attempt {attempt + 1}/{max_attempts}): {error}; retrying in {delay}s...",
                file=sys.stderr,
            )
            time.sleep(delay)

    # Should not reach here, but raise last error just in case
    if last_error:
        raise last_error


def _download(url: str, destination: Path) -> None:
    """Download a file with retry-with-backoff on transient errors."""

    def download_attempt():
        request = Request(url, headers={"User-Agent": "polylint-python-wrapper"})
        context = ssl.create_default_context(cafile=certifi.where())
        try:
            with urlopen(request, timeout=30, context=context) as response:
                if response.status != 200:
                    raise RuntimeError(f"HTTP {response.status}: {response.reason}")
                destination.write_bytes(response.read())
        except URLError as exc:
            raise RuntimeError(f"Failed to download binary: {exc}") from exc

    _retry_with_backoff(download_attempt)


def _download_text(url: str) -> str:
    """Download text content with retry-with-backoff on transient errors."""

    def download_attempt():
        request = Request(url, headers={"User-Agent": "polylint-python-wrapper"})
        context = ssl.create_default_context(cafile=certifi.where())
        try:
            with urlopen(request, timeout=30, context=context) as response:
                if response.status != 200:
                    raise RuntimeError(f"HTTP {response.status}: {response.reason}")
                return response.read().decode("utf-8")
        except URLError as exc:
            raise RuntimeError(f"Failed to download checksums: {exc}") from exc

    return _retry_with_backoff(download_attempt)


def _expected_digest(checksums_text: str, asset_name: str) -> str | None:
    """Find the sha256 digest for asset_name in a `sha256<space>filename` file.

    Entries may carry a leading ``./`` path prefix and/or a ``*`` binary-mode
    marker on the filename; both are stripped before matching the asset name.
    """
    for line in checksums_text.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        parts = stripped.split()
        if len(parts) < 2:
            continue
        # GNU coreutils binary-mode marks the name with a leading '*'; release
        # checksum files often list the path as './<asset>'.
        name = parts[-1].lstrip("*")
        if name.startswith("./"):
            name = name[2:]
        if name == asset_name:
            return parts[0].lower()
    return None


def _verify_checksum(archive: Path, asset_name: str, checksums_url: str) -> None:
    """Verify the archive sha256 against the release checksums file.

    Fails CLOSED: any failure to fetch the checksums, locate the entry, or
    match the digest raises, aborting the install rather than continuing with
    an unverified binary.
    """
    try:
        checksums_text = _download_text(checksums_url)
    except RuntimeError as exc:
        raise RuntimeError(
            f"could not fetch checksums ({checksums_url}): {exc} — refusing to install unverified binary"
        ) from exc

    expected = _expected_digest(checksums_text, asset_name)
    if not expected:
        raise RuntimeError(
            f"no checksum entry for {asset_name} in {checksums_url} — refusing to install unverified binary"
        )

    digest = hashlib.sha256()
    digest.update(archive.read_bytes())
    actual = digest.hexdigest().lower()
    if actual != expected:
        raise RuntimeError(f"checksum mismatch for {asset_name} (expected {expected}, got {actual})")


def _extract(archive: Path, ext: str, destination: Path) -> None:
    """Extract the full archive tree into destination."""
    if ext == "zip":
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(destination)
    else:
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(destination)


def _binary_name(base: str) -> str:
    return f"{base}.exe" if platform.system().lower() == "windows" else base


def _cache_dir(version: str) -> Path:
    """Directory holding the extracted binary for this version."""
    cache_dir = Path.home() / ".cache" / "polylint" / version
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir


def _binary_path(cache_dir: Path, base: str) -> Path:
    return cache_dir / _binary_name(base)


def _all_binaries_present(cache_dir: Path) -> bool:
    return all(
        _binary_path(cache_dir, base).exists() and os.access(_binary_path(cache_dir, base), os.X_OK)
        for base in BINARIES
    )


def ensure_binaries() -> Path:
    """Ensure the poly binary is available, downloading if necessary.

    Returns the cache directory containing ``poly``.
    Handles concurrent invocations via atomic rename: download+extract into a
    temp dir, then atomically move into the cache to prevent corruption from
    parallel installs.
    """
    from . import __version__

    cache_dir = _cache_dir(__version__)
    if _all_binaries_present(cache_dir):
        return cache_dir

    archive_url, ext, asset_name, checksums_url = _asset(__version__)
    print(f"Downloading poly binary v{__version__}...", file=sys.stderr)

    # Atomic install strategy:
    # 1. Download + extract into a temp directory (not under cache_dir)
    # 2. Atomically rename the temp extraction into the versioned cache path
    # 3. Use a simple lock file to serialize concurrent downloads of the same version
    lock_path = cache_dir / ".lock"
    cache_dir.mkdir(parents=True, exist_ok=True)

    # Try to acquire lock via exclusive file creation (atomic, works cross-platform)
    lock_acquired = False
    try:
        # O_CREAT | O_EXCL: atomic, fails if lock already exists
        lock_fd = os.open(str(lock_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        lock_acquired = True
        os.close(lock_fd)
    except FileExistsError as exc:
        # Another process holds the lock. Wait for it to complete, then check if binaries exist.
        for _ in range(30):  # Wait up to 30 seconds for the other process
            time.sleep(0.1)
            if _all_binaries_present(cache_dir):
                return cache_dir
        raise RuntimeError(
            f"Timeout waiting for concurrent binary installation of {__version__}. "
            f"If this persists, remove {cache_dir} and retry."
        ) from exc

    try:
        # Double-check: another process may have installed while we were waiting for the lock
        if _all_binaries_present(cache_dir):
            return cache_dir

        # Download and extract into a temporary directory outside the cache
        with tempfile.TemporaryDirectory() as tmpdir:
            archive_path = Path(tmpdir) / asset_name
            _download(archive_url, archive_path)
            # Fail CLOSED: verify before extracting anything into the cache.
            _verify_checksum(archive_path, asset_name, checksums_url)

            # Extract into a temporary staging directory
            staging_dir = Path(tmpdir) / "staging"
            staging_dir.mkdir()
            _extract(archive_path, ext, staging_dir)

            # Atomic rename: move the staged extraction into the cache.
            try:
                staging_dir.replace(cache_dir)
            except (OSError, FileExistsError):
                # cache_dir already exists. Either another process won the race and the
                # runnable binaries are present (use them), or it is a stale/partial dir.
                # os.replace() cannot overwrite a non-empty directory (raises Errno 66),
                # so under the lock we hold here, clear the stale dir and retry the move.
                if not _all_binaries_present(cache_dir):
                    shutil.rmtree(cache_dir, ignore_errors=True)
                    staging_dir.replace(cache_dir)

        missing = [base for base in BINARIES if not _binary_path(cache_dir, base).exists()]
        if missing:
            raise RuntimeError(f"binaries {missing} not found after extracting {asset_name}")

        if platform.system().lower() != "windows":
            for base in BINARIES:
                _binary_path(cache_dir, base).chmod(0o755)

        print("Binary downloaded successfully!", file=sys.stderr)
        return cache_dir
    finally:
        if lock_acquired:
            try:
                lock_path.unlink()
            except FileNotFoundError:
                pass  # Lock already cleaned up, ok


def ensure_binary(base: str) -> str:
    """Ensure the named poly binary is available.

    Honours the ``POLYLINT_BINARY_<BASE>`` and ``POLYLINT_BINARY`` overrides (the
    latter pointing at the cache directory), self-healing by downloading the
    release archive when the binary is absent.
    """
    override = os.getenv(f"POLYLINT_BINARY_{base.upper()}")
    if override:
        return override

    cache_override = os.getenv("POLYLINT_BINARY")
    cache_dir = Path(cache_override) if cache_override else None

    if cache_dir is None:
        from . import __version__

        cache_dir = _cache_dir(__version__)

    binary_path = _binary_path(cache_dir, base)
    if binary_path.exists() and os.access(binary_path, os.X_OK):
        return str(binary_path)

    # Self-heal: download (once) and retry.
    cache_dir = ensure_binaries()
    binary_path = _binary_path(cache_dir, base)
    if binary_path.exists() and os.access(binary_path, os.X_OK):
        return str(binary_path)

    raise RuntimeError(f"binary {_binary_name(base)} not found after install")


def run_binary(base: str, args) -> None:
    """Run the poly binary with the given arguments."""
    binary_path = ensure_binary(base)

    try:
        result = subprocess.run([binary_path] + list(args), check=False)
        sys.exit(result.returncode)
    except FileNotFoundError as exc:
        raise RuntimeError(f"Binary not found at {binary_path}") from exc
    except Exception as exc:
        raise RuntimeError(f"Failed to run {base}: {exc}") from exc
