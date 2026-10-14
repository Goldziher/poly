"""Verify release version synchronization against real native plugin generation."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import tomllib


class ReleaseBumpTests(unittest.TestCase):
    """Exercise isolated copied sources without modifying the checkout."""

    def setUp(self) -> None:
        """Copy only release inputs; build artifacts and caches are not required."""
        temporary = tempfile.TemporaryDirectory(prefix="poly-release-contract-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        source = Path(__file__).resolve().parents[2]
        for directory in [".ai-rulez", "plugin/.ai-rulez", "npm-package", "pip-package", "scripts"]:
            shutil.copytree(
                source / directory,
                self.root / directory,
                ignore=shutil.ignore_patterns("node_modules", "bin", "dist", "*.tgz", "__pycache__"),
            )
        for filename in ["Cargo.toml", "poly.toml", "package.json"]:
            shutil.copy2(source / filename, self.root / filename)

    def run_bump(self, version: str) -> subprocess.CompletedProcess[str]:
        """Use the actual CLI with Cargo dependency updates deliberately disabled."""
        return subprocess.run(
            ["bash", "scripts/release-bump.sh", version],
            cwd=self.root,
            env={**os.environ, "SKIP_CARGO_UPDATE": "1"},
            capture_output=True,
            text=True,
            check=False,
        )

    def test_should_bump_every_product_bundle_without_requiring_an_index_version(self) -> None:
        """The native root marketplace resolves a member instead of duplicating its version."""
        result = self.run_bump("0.29.1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        author = tomllib.loads((self.root / "plugin/.ai-rulez/config.plugins.toml").read_text())
        self.assertEqual(author["plugin"]["version"], "0.29.1")
        self.assertEqual(len(author["plugin"]["runtimes"]), 11)
        package = json.loads((self.root / "plugin/package.json").read_text())
        self.assertEqual(package["version"], "0.29.1")
        self.assertEqual(len(package["pi"]["skills"]), 5)
        self.assertEqual(len(package["pi"]["prompts"]), 2)
        marketplace = json.loads((self.root / ".claude-plugin/marketplace.json").read_text())
        self.assertEqual(marketplace["plugins"][0]["source"], "./plugin")
        self.assertNotIn("version", marketplace["plugins"][0])

    def test_should_reject_malformed_versions_before_mutating_inputs(self) -> None:
        """A rejected argument must preserve the authoritative core version source."""
        before = (self.root / "Cargo.toml").read_bytes()
        result = self.run_bump("bad-version")
        self.assertEqual(result.returncode, 1)
        self.assertIn("version must be MAJOR.MINOR.PATCH", result.stderr)
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), before)
