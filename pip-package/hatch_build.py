"""Hatchling build hook that turns the wheel into a platform wheel.

``build_wheels.py`` invokes the backend once per release target with
``POLY_WHEEL_TAG`` and ``POLY_WHEEL_BINARY`` set; this hook stamps the resulting
wheel with that platform tag and force-includes the binary. With neither set it
does nothing, which produces the pure-Python ``py3-none-any`` fallback wheel.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from hatchling.builders.hooks.plugin.interface import BuildHookInterface


class CustomBuildHook(BuildHookInterface):
    """Stamp the wheel with a platform tag and embed the prebuilt binary."""

    PLUGIN_NAME = "custom"

    def initialize(self, version: str, build_data: dict[str, Any]) -> None:
        tag = os.environ.get("POLY_WHEEL_TAG")
        binary = os.environ.get("POLY_WHEEL_BINARY")

        if not tag and not binary:
            return

        if not (tag and binary):
            message = "POLY_WHEEL_TAG and POLY_WHEEL_BINARY must be set together"
            raise ValueError(message)

        if not Path(binary).is_file():
            message = f"POLY_WHEEL_BINARY does not exist: {binary}"
            raise FileNotFoundError(message)

        target_name = "poly.exe" if tag.startswith("win") or "win_" in tag else "poly"

        build_data["pure_python"] = False
        build_data["infer_tag"] = False
        build_data["tag"] = f"py3-none-{tag}"
        build_data["force_include"][binary] = f"polylint/bin/{target_name}"
