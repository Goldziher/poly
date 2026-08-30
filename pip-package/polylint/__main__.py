"""Console-script shim that hands off to the prebuilt ``poly`` binary.

Both the ``poly`` and ``polylint`` entry points call :func:`main`; ``poly`` is
the canonical command and ``polylint`` is an alias for it, matching the name the
distribution is published under.

Distribution model: this project ships **platform-specific wheels**, each
carrying exactly one prebuilt binary under ``polylint/bin/``. Nothing is
downloaded at install time, so installs work offline and the binary is covered
by the wheel hash pip already verifies.
"""

# poly: allow-file[T201] this shim is a CLI entry point; stderr is its only output channel.

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

#: Directory the platform wheel unpacks the binary into.
_BIN_DIR = Path(__file__).resolve().parent / "bin"

#: Executable name inside that directory.
_BINARY_NAME = "poly.exe" if os.name == "nt" else "poly"

#: Where to point a user whose platform has no wheel.
_INSTALL_DOCS = "https://github.com/Goldziher/poly#installation"


def binary_path() -> Path | None:
    """Return the bundled ``poly`` executable, or ``None`` if this wheel has none.

    A ``None`` here means the *pure-Python fallback wheel* was installed: it is
    published as ``py3-none-any`` so that an unsupported platform gets an
    explanatory command rather than pip's bare "could not find a version that
    satisfies the requirement". pip ranks platform-specific wheels above
    ``any``, so a supported platform never resolves to it.
    """
    candidate = _BIN_DIR / _BINARY_NAME
    return candidate if candidate.is_file() else None


def _unsupported_platform_message() -> str:
    return (
        f"poly: this install of polylint {_version()} carries no binary for "
        f"{sys.platform}/{os.uname().machine if hasattr(os, 'uname') else 'unknown'}.\n\n"
        "Prebuilt wheels are published for macOS (x86_64, arm64), Linux x86_64 "
        "(glibc and musl), Linux aarch64 (glibc), and Windows x86_64. If you are "
        "on one of those, your glibc may be older than the wheels require.\n\n"
        f"Install poly directly instead: {_INSTALL_DOCS}"
    )


def _version() -> str:
    from polylint import __version__

    return __version__


def main() -> None:
    """Run the bundled binary with this process's arguments and exit with its status."""
    binary = binary_path()
    if binary is None:
        print(_unsupported_platform_message(), file=sys.stderr)
        raise SystemExit(1)

    argv = [str(binary), *sys.argv[1:]]

    if os.name == "nt":
        # Windows has no exec that replaces the process in a way Ctrl-C and job
        # control survive, so spawn and forward the status instead.
        try:
            completed = subprocess.run(argv, check=False)  # noqa: S603 - argv is fully controlled
        except OSError as exc:
            print(f"poly: failed to run {binary}: {exc}", file=sys.stderr)
            raise SystemExit(1) from exc
        raise SystemExit(completed.returncode)

    # execv replaces this process, so poly's exit codes (0 clean, 1 findings,
    # 2 the run verified less than it claims) and any signal that kills it reach
    # the caller exactly as they would from the standalone binary. A wrapper that
    # spawned and re-raised would be one more layer to get wrong.
    try:
        os.execv(str(binary), argv)
    except OSError as exc:
        print(f"poly: failed to exec {binary}: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc


if __name__ == "__main__":
    main()
