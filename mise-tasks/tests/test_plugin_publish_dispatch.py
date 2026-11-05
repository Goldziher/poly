import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/publish.yaml"


class PluginDispatchTests(unittest.TestCase):
    def test_plugin_only_mode_is_guarded_and_uses_dispatch_commit(self) -> None:
        text = WORKFLOW.read_text()
        self.assertIn("plugins-only:", text)
        self.assertIn("if: steps.npm-version.outputs.exists != 'true'", text)
        blocks = dict(
            re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)", text.split("jobs:\n", 1)[1], re.MULTILINE | re.DOTALL)
        )
        self.assertGreater(len(blocks), 8)
        for name, block in blocks.items():
            if name == "plugin-meta" or "report" in name:
                continue
            if "plugin" in name and ("opencode" in name or "npm" in name or "hermes" in name):
                self.assertIn("plugin-meta", block)
                self.assertIn("needs.plugin-meta.result", block)
                self.assertIn("inputs.plugins-only && github.sha", block)
                self.assertIn("actions/publish-", block)
            else:
                self.assertIn("!inputs.plugins-only", block, name)

    def test_registry_payload_guards_run_before_each_upload_or_skip(self) -> None:
        text = WORKFLOW.read_text()
        blocks = dict(
            re.findall(r"^  ([\w-]+):\n(.*?)(?=^  [\w-]+:|\Z)", text.split("jobs:\n", 1)[1], re.MULTILINE | re.DOTALL)
        )
        cases = [
            ("publish_npm", "npm", 'if [ "$verdict" = "exists=true" ]'),
            ("publish_plugin_npm", "npm", "if: steps.npm-version.outputs.exists"),
            ("publish_plugin_hermes", "wheel", "uses: xberg-io/actions/publish-pypi@v1"),
            ("publish_pypi", "wheel", "uses: pypa/gh-action-pypi-publish@v1.14.3"),
        ]
        self.assertEqual(len(cases), 4)
        for name, kind, boundary in cases:
            with self.subTest(job=name):
                block = blocks[name]
                guard = 'python3 "$POLY_REGISTRY_GUARD" ' + kind
                self.assertEqual(block.count(guard), 1)
                self.assertLess(block.index(guard), block.index(boundary))
                self.assertNotIn("continue-on-error", block)
                self.assertNotIn("pypi_exists", block)
                self.assertNotIn("npm_bootstrapped", block)
        native = blocks["publish_npm"]
        self.assertNotIn("pnpm view", native)
        self.assertLess(native.index('"$POLY_REGISTRY_GUARD"'), native.index('pnpm publish "$archive"'))
        self.assertNotIn("Detect registry readiness", text)
        self.assertEqual(text.count("&registry-controller"), 1)
        self.assertEqual(text.count("*registry-controller"), 3)
        self.assertIn("CONTROLLER_SHA: ${{ github.workflow_sha }}", text)
        self.assertIn('git fetch --no-tags --no-write-fetch-head origin "$CONTROLLER_SHA"', text)
        self.assertIn('git show "$CONTROLLER_SHA:mise-tasks/verify/$helper"', text)
        self.assertIn("registry-payload.py registry_archive.py", text)
        for name, _, _ in cases:
            self.assertIn("registry-controller", blocks[name])
            self.assertIn('python3 "$POLY_REGISTRY_GUARD"', blocks[name])
        self.assertIn("wheel --expected-count 1 dist-hermes/*.whl", blocks["publish_plugin_hermes"])
        self.assertIn("wheel --expected-count 7 pip-package/dist/*.whl", blocks["publish_pypi"])
        self.assertLess(native.index("Incomplete npm platform inventory"), native.index('for dir in "${platforms[@]}"'))

    def test_metadata_rejects_version_mismatch_and_unsafe_ref(self) -> None:
        text = WORKFLOW.read_text()
        block = text.split("  plugin-meta:\n", 1)[1].split("  meta:\n", 1)[0]
        script = block.split("          python3 - <<'PY'\n", 1)[1].split("          PY", 1)[0]
        script = "\n".join(line[10:] for line in script.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "plugin/.hermes/package").mkdir(parents=True)
            (root / "Cargo.toml").write_text('[package]\nversion = "1.2.3"\n')
            (root / "plugin/package.json").write_text('{"version":"1.2.3"}')
            manifest = root / "plugin/.hermes/package/pyproject.toml"
            manifest.write_text('[project]\nversion = "1.2.3"\n')
            env = dict(os.environ, GITHUB_OUTPUT=str(root / "output"), DISPATCH_REF="refs/heads/main")
            result = subprocess.run(["python3", "-c", script], cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / "output").read_text(), "version=1.2.3\n")
            manifest.write_text('[project]\nversion = "1.2.4"\n')
            result = subprocess.run(["python3", "-c", script], cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Plugin versions disagree", result.stderr)
            env["DISPATCH_REF"] = "refs/heads/other"
            result = subprocess.run(["python3", "-c", script], cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires main", result.stderr)


if __name__ == "__main__":
    unittest.main()
