"""CLI entry point for the poly binary."""

import sys

from .downloader import run_binary


def _run(binary_base_name: str) -> None:
    """Resolve and run the named binary with the current process arguments."""
    args = sys.argv[1:]
    run_binary(binary_base_name, args)


def poly() -> None:
    """Entry point for the ``poly`` umbrella CLI."""
    _run("poly")


if __name__ == "__main__":
    poly()
