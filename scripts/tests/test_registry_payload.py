"""Exercise immutable registry parity against complete real archive payloads."""

import base64
import contextlib
import csv
import hashlib
import importlib.util
import io
import json
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import urllib.error
import zipfile
from collections.abc import Callable
from pathlib import Path
from types import ModuleType
from typing import NoReturn
from unittest.mock import patch


def load_guard() -> ModuleType:
    path = Path(__file__).resolve().parents[1] / "verify_registry_payload.py"
    spec = importlib.util.spec_from_file_location("registry_guard", path)
    module = importlib.util.module_from_spec(spec)
    with patch.object(sys, "path", [str(path.parent), *sys.path]):
        spec.loader.exec_module(module)
    return module


def npm_archive(dependency: bool = False, text: bytes = b"adapter", timestamp: int = 1, reverse: bool = False) -> bytes:
    manifest = {"name": "@example/plugin", "version": "1.2.3", "dependencies": {}}
    if dependency:
        manifest["dependencies"] = {"@opencode/plugin": "^2.0.26"}
    files = {"package/package.json": json.dumps(manifest).encode(), "package/adapter.js": text}
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name, data in sorted(files.items(), reverse=reverse):
            member = tarfile.TarInfo(name)
            member.size, member.mode, member.mtime = len(data), 0o644, timestamp
            archive.addfile(member, io.BytesIO(data))
    return output.getvalue()


def wheel_archive(
    text: bytes = b"adapter",
    timestamp: tuple[int, ...] = (2020, 1, 1, 0, 0, 0),
    corrupt_record: bool = False,
    reverse: bool = False,
    requires: bytes = b"Requires-Dist: mcp==1.0\n",
) -> bytes:
    prefix = "example-1.2.3.dist-info/"
    files = {
        "example/__init__.py": text,
        prefix + "METADATA": b"Name: example\nVersion: 1.2.3\n" + requires,
        prefix + "WHEEL": b"Wheel-Version: 1.0\nTag: py3-none-any\n",
    }
    rows = []
    for name, data in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("=")
        rows.append([name, "sha256=" + digest, str(len(data))])
    rows.append([prefix + "RECORD", "", ""])
    record = io.StringIO()
    csv.writer(record).writerows(rows[::-1] if reverse else rows)
    files[prefix + "RECORD"] = record.getvalue().encode()
    if corrupt_record:
        files["example/__init__.py"] += b"poison"
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for name, data in sorted(files.items(), reverse=reverse):
            info = zipfile.ZipInfo(name, timestamp)
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    return output.getvalue()


class RegistryPayloadTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.guard = load_guard()

    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def candidate(self, data: bytes, name: str = "candidate.tgz") -> Path:
        path = self.root / name
        path.write_bytes(data)
        return path

    def npm_fetch(self, published: bytes) -> Callable[[str], bytes]:
        metadata = {
            "name": "@example/plugin",
            "version": "1.2.3",
            "dist": {
                "tarball": "https://registry.npmjs.org/payload",
                "integrity": "sha512-" + base64.b64encode(hashlib.sha512(published).digest()).decode(),
            },
        }
        return lambda url: published if url.endswith("/payload") else json.dumps(metadata).encode()

    def wheel_fetch(self, published: bytes, name: str = "candidate.whl") -> Callable[[str], bytes]:
        metadata = {
            "info": {"name": "example", "version": "1.2.3"},
            "urls": [
                {
                    "filename": name,
                    "url": "https://files.pythonhosted.org/payload",
                    "digests": {"sha256": hashlib.sha256(published).hexdigest()},
                }
            ],
        }
        return lambda url: published if url.endswith("/payload") else json.dumps(metadata).encode()

    def controller_probe(self, kind: str, candidate: Path, published: bytes) -> subprocess.CompletedProcess[str]:
        product, controller = self.root / "older-product", self.root / "controller"
        product.mkdir(exist_ok=True)
        (product / candidate.name).write_bytes(candidate.read_bytes())
        controller.mkdir(exist_ok=True)
        for name in ["verify_registry_payload.py", "registry_archive.py"]:
            shutil.copyfile(Path(__file__).resolve().parents[1] / name, controller / name)
        self.assertFalse((product / "scripts/verify_registry_payload.py").exists())
        artifact, metadata = self.root / "remote.archive", self.root / "registry.json"
        artifact.write_bytes(published)
        fetch = self.npm_fetch(published) if kind == "npm" else self.wheel_fetch(published, candidate.name)
        metadata.write_bytes(fetch("https://example.test/metadata"))
        bootstrap = """
import io, runpy, sys, urllib.request
from pathlib import Path
guard, kind, candidate, metadata, artifact = sys.argv[1:]
sys.path.insert(0, str(Path(guard).parent))
requests = []
def open_url(request, timeout):
    requests.append(request.full_url)
    response = io.BytesIO(Path(artifact if request.full_url.endswith('/payload') else metadata).read_bytes())
    response.geturl = lambda: request.full_url
    return response
urllib.request.urlopen = open_url
sys.argv = [guard, kind]
if kind == 'wheel':
    sys.argv += ['--expected-count', '1']
sys.argv += [candidate]
runpy.run_path(guard, run_name='__main__')
assert len(requests) == 2
"""
        return subprocess.run(
            [
                sys.executable,
                "-c",
                bootstrap,
                str(controller / "verify_registry_payload.py"),
                kind,
                candidate.name,
                str(metadata),
                str(artifact),
            ],
            cwd=product,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_npm_timestamp_and_order_changes_preserve_legitimate_reruns(self) -> None:
        path = self.candidate(npm_archive())
        self.assertTrue(self.guard.check_npm(path, self.npm_fetch(npm_archive(timestamp=900, reverse=True))))

    def test_npm_sdk_dependency_drift_is_an_immutable_conflict(self) -> None:
        path = self.candidate(npm_archive())
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_npm(path, self.npm_fetch(npm_archive(dependency=True)))

    def test_npm_adapter_or_native_binary_drift_is_an_immutable_conflict(self) -> None:
        path = self.candidate(npm_archive())
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_npm(path, self.npm_fetch(npm_archive(text=b"different native bytes")))

    def test_wheel_timestamp_order_and_record_row_order_preserve_reruns(self) -> None:
        path = self.candidate(wheel_archive(), "candidate.whl")
        remote = wheel_archive(timestamp=(2024, 1, 1, 0, 0, 0), reverse=True)
        self.assertTrue(self.guard.check_wheels([path], self.wheel_fetch(remote)))

    def test_changed_wheel_resource_is_an_immutable_conflict(self) -> None:
        path = self.candidate(wheel_archive(), "candidate.whl")
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_wheels([path], self.wheel_fetch(wheel_archive(text=b"stale adapter")))

    def test_wheel_sdk_dependency_drift_is_an_immutable_conflict(self) -> None:
        path = self.candidate(wheel_archive(), "candidate.whl")
        changed = wheel_archive(requires=b"Requires-Dist: mcp==2.0\n")
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_wheels([path], self.wheel_fetch(changed))

    def test_invalid_wheel_record_is_rejected_before_registry_access(self) -> None:
        path = self.candidate(wheel_archive(corrupt_record=True), "candidate.whl")
        with self.assertRaisesRegex(ValueError, "RECORD"):
            self.guard.check_wheels([path], lambda _url: self.fail("bad candidate reached registry"))

    def test_only_metadata_404_means_the_version_is_missing(self) -> None:
        def absent(url: str) -> NoReturn:
            raise urllib.error.HTTPError(url, 404, "missing", {}, io.BytesIO())

        self.assertFalse(self.guard.check_npm(self.candidate(npm_archive()), absent))
        self.assertFalse(self.guard.check_wheels([self.candidate(wheel_archive(), "candidate.whl")], absent))

    def test_registry_and_transport_errors_fail_closed(self) -> None:
        for error in [urllib.error.HTTPError("url", 500, "failed", {}, io.BytesIO()), urllib.error.URLError("offline")]:

            def failed(_url: str, error: Exception = error) -> NoReturn:
                raise error

            with self.subTest(error=error), self.assertRaises((urllib.error.HTTPError, urllib.error.URLError)):
                self.guard.check_npm(self.candidate(npm_archive()), failed)

    def test_malformed_registry_json_or_identity_is_rejected(self) -> None:
        bodies = [
            b"invalid",
            b"{}",
            b"[]",
            b'{"name":"wrong","version":"1.2.3"}',
            b'{"name":"first","name":"second"}',
            b'{"invalid":NaN}',
        ]
        for body in bodies:
            with self.subTest(body=body), self.assertRaises((ValueError, TypeError)):
                self.guard.check_npm(self.candidate(npm_archive()), lambda _url, body=body: body)

    def test_empty_or_malformed_archive_inventories_are_rejected(self) -> None:
        for payload in [b"", b"not an archive"]:
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                self.guard.check_npm(self.candidate(payload), lambda _url: self.fail("bad archive reached registry"))
        with self.assertRaisesRegex(ValueError, "No candidate"):
            self.guard.check_wheels([], lambda _url: self.fail("empty list reached registry"))

    def test_duplicate_and_traversing_tar_members_are_rejected(self) -> None:
        for names in [["package/package.json", "package/package.json"], ["package/../escape"]]:
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:gz") as archive:
                for name in names:
                    member = tarfile.TarInfo(name)
                    member.size = 2
                    archive.addfile(member, io.BytesIO(b"{}"))
            with self.subTest(names=names), self.assertRaises(ValueError):
                self.guard.check_npm(self.candidate(output.getvalue()), lambda _url: self.fail("unsafe archive"))

    def test_remote_digest_mismatch_is_rejected(self) -> None:
        remote = npm_archive()
        fetch = self.npm_fetch(remote)
        with self.assertRaisesRegex(ValueError, "digest"):
            self.guard.check_npm(
                self.candidate(remote), lambda url: b"poison" if url.endswith("/payload") else fetch(url)
            )

    def test_partial_wheel_publication_verifies_existing_payload_before_missing_uploads(self) -> None:
        payload = wheel_archive()
        first = self.candidate(payload, "candidate.whl")
        second = self.candidate(payload, "additional.whl")
        self.assertFalse(self.guard.check_wheels([first, second], self.wheel_fetch(payload)))
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_wheels([first, second], self.wheel_fetch(wheel_archive(text=b"stale")))

    def test_empty_unexpected_or_duplicate_pypi_inventories_are_rejected(self) -> None:
        path = self.candidate(wheel_archive(), "candidate.whl")
        for names in [[], ["unexpected.whl"], ["candidate.whl", "candidate.whl"]]:
            metadata = {"info": {"name": "example", "version": "1.2.3"}, "urls": [{"filename": name} for name in names]}
            with self.subTest(names=names), self.assertRaises(ValueError):
                self.guard.check_wheels([path], lambda _url, metadata=metadata: json.dumps(metadata).encode())
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            self.guard.check_wheels([path, path], lambda _url: self.fail("duplicate candidates reached registry"))

    def test_missing_remote_artifact_is_fatal_after_successful_metadata(self) -> None:
        payload = npm_archive()
        metadata = self.npm_fetch(payload)

        def fetch(url: str) -> bytes:
            if url.endswith("/payload"):
                raise urllib.error.HTTPError(url, 404, "artifact missing", {}, io.BytesIO())
            return metadata(url)

        with self.assertRaises(urllib.error.HTTPError) as caught:
            self.guard.check_npm(self.candidate(payload), fetch)
        caught.exception.close()

    def test_valid_empty_tar_and_zip_inventories_are_rejected(self) -> None:
        for kind in ["npm", "wheel"]:
            output = io.BytesIO()
            if kind == "npm":
                with tarfile.open(fileobj=output, mode="w:gz"):
                    pass
                parse = self.guard.npm_payload
            else:
                with zipfile.ZipFile(output, "w"):
                    pass
                parse = self.guard.wheel_payload
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, "Empty"):
                parse(output.getvalue())

    def test_native_executable_permission_drift_is_rejected(self) -> None:
        payload = npm_archive()
        output = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(payload)) as source, tarfile.open(fileobj=output, mode="w:gz") as target:
            for member in source:
                member.mode = 0o755
                target.addfile(member, source.extractfile(member))
        with self.assertRaisesRegex(ValueError, "Immutable payload conflict"):
            self.guard.check_npm(self.candidate(payload), self.npm_fetch(output.getvalue()))

    def test_duplicate_traversing_and_symlink_wheel_members_are_rejected(self) -> None:
        inventories = [
            [("example/module.py", 0o100644), ("example/module.py", 0o100644)],
            [("example/../escape", 0o100644)],
            [("example/link", 0o120777)],
        ]
        for members in inventories:
            output = io.BytesIO()
            with zipfile.ZipFile(output, "w") as archive:
                for name, mode in members:
                    info = zipfile.ZipInfo(name)
                    info.external_attr = mode << 16
                    with self.assertWarns(UserWarning) if name in archive.namelist() else contextlib.nullcontext():
                        archive.writestr(info, b"payload")
            with self.subTest(members=members), self.assertRaises(ValueError):
                self.guard.wheel_payload(output.getvalue())

    def test_cli_emits_no_success_verdict_on_transport_or_missing_candidate_failure(self) -> None:
        path = self.candidate(npm_archive())
        for candidate in [path, self.root / "missing.tgz"]:
            output = io.StringIO()
            with (
                patch.object(sys, "argv", ["guard", "npm", str(candidate)]),
                contextlib.redirect_stdout(output),
                patch("urllib.request.urlopen", side_effect=urllib.error.URLError("offline")) as request,
            ):
                with self.assertRaises((ValueError, urllib.error.URLError)):
                    self.guard.main()
                self.assertEqual(request.call_count, int(candidate == path))
            self.assertEqual(output.getvalue(), "")

    def test_cli_rejects_missing_or_incomplete_expected_wheel_census_before_registry_access(self) -> None:
        path = self.candidate(wheel_archive(), "candidate.whl")
        for count in [[], ["--expected-count", "0"], ["--expected-count", "2"]]:
            with self.subTest(count=count), patch.object(sys, "argv", ["guard", "wheel", *count, str(path)]):
                with patch("urllib.request.urlopen") as request, self.assertRaisesRegex(ValueError, "count mismatch"):
                    self.guard.main()
                request.assert_not_called()

    def test_controller_guard_checks_older_product_contract_without_product_helpers(self) -> None:
        cases = [
            ("npm", npm_archive(dependency=True), npm_archive(), "older.tgz"),
            ("wheel", wheel_archive(requires=b"Requires-Dist: mcp==2.0\n"), wheel_archive(), "older.whl"),
        ]
        for kind, old_sdk, current_sdk, name in cases:
            path = self.candidate(old_sdk, name)
            for published, expected in [(old_sdk, 0), (current_sdk, 1)]:
                with self.subTest(kind=kind, expected=expected):
                    result = self.controller_probe(kind, path, published)
                    self.assertEqual(result.returncode, expected, result.stderr)
                    if expected == 0:
                        self.assertEqual(result.stdout, "exists=true\n")
                    else:
                        self.assertEqual(result.stdout, "")
                        self.assertIn("Immutable payload conflict:", result.stderr)


if __name__ == "__main__":
    unittest.main()
