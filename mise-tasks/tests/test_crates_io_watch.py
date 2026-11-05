"""Check the crates.io watch against fake registry answers, without network."""

import contextlib
import importlib.util
import io
import json
import tomllib
import unittest
import urllib.error
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
_spec = importlib.util.spec_from_file_location("crates_io_watch", ROOT / "mise-tasks/crates-io/watch.py")
assert _spec is not None and _spec.loader is not None
watch = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(watch)

# Published crates that stay git-pinned only because they must move in lockstep with the blocked
# oxc/biome crates: mixing a registry copy with a git copy is a hard type-mismatch compile error.
LOCKSTEP = {
    "oxc_allocator",
    "oxc_diagnostics",
    "oxc_parser",
    "oxc_semantic",
    "oxc_span",
    "biome_rowan",
    "biome_diagnostics",
}


class FakeResponse(io.BytesIO):
    """A urlopen result usable as a context manager."""


def versions_payload(*entries: tuple[str, bool]) -> FakeResponse:
    body = {"versions": [{"num": num, "yanked": yanked} for num, yanked in entries]}
    return FakeResponse(json.dumps(body).encode())


def http_error(code: int) -> urllib.error.HTTPError:
    return urllib.error.HTTPError("https://crates.io", code, "x", {}, None)  # type: ignore[arg-type]


class ReleaseLogicTests(unittest.TestCase):
    """real_releases and check decide what counts as a release."""

    def test_placeholders_do_not_count(self) -> None:
        self.assertEqual(watch.real_releases([("0.0.0", False), ("0.0.0-wip", False)], None), ([], []))

    def test_yanked_only_does_not_count(self) -> None:
        self.assertEqual(watch.real_releases([("1.0.0", True), ("1.1.0", True)], None), ([], []))

    def test_real_version_counts(self) -> None:
        self.assertEqual(watch.real_releases([("0.1.0", False)], None), (["0.1.0"], []))

    def test_prerelease_is_notice_not_release(self) -> None:
        self.assertEqual(watch.real_releases([("1.0.0-rc.1", False)], None), ([], ["1.0.0-rc.1"]))

    def test_stale_baseline_must_be_exceeded(self) -> None:
        self.assertEqual(watch.real_releases([("0.5.7", False)], "0.5.7"), ([], []))
        self.assertEqual(watch.real_releases([("0.5.6", False)], "0.5.7"), ([], []))
        # A newer patch above the stale baseline is a real release: upstream resumed publishing.
        self.assertEqual(watch.real_releases([("0.5.7", False), ("0.5.9", False)], "0.5.7"), (["0.5.9"], []))

    def test_check_reports_release_and_notice(self) -> None:
        state = {
            "biome_analyze": [("0.5.7", False), ("0.5.9", False)],
            "oxc_linter": [("2.0.0-alpha.1", False)],
            "rubyfmt": [("0.0.0-wip", False)],
        }
        errors, notices = watch.check(
            {"biome_analyze": "0.5.7", "oxc_linter": None, "rubyfmt": None, "oxc_formatter_core": None},
            lambda name: state.get(name),
        )
        self.assertEqual(len(errors), 1)
        self.assertIn("biome_analyze 0.5.9", errors[0])
        self.assertEqual(len(notices), 1)
        self.assertIn("oxc_linter", notices[0])


class FetchTests(unittest.TestCase):
    """fetch_versions through a patched urlopen."""

    def test_parses_versions(self) -> None:
        with mock.patch("urllib.request.urlopen", return_value=versions_payload(("1.0.0", True), ("1.1.0", False))):
            self.assertEqual(watch.fetch_versions("x", sleep=lambda _: None), [("1.0.0", True), ("1.1.0", False)])

    def test_404_is_not_published(self) -> None:
        with mock.patch("urllib.request.urlopen", side_effect=http_error(404)) as opened:
            self.assertIsNone(watch.fetch_versions("x", sleep=lambda _: None))
        self.assertEqual(opened.call_count, 1)

    def test_500_retries_then_unreachable(self) -> None:
        with (
            mock.patch("urllib.request.urlopen", side_effect=http_error(500)) as opened,
            self.assertRaises(watch.Unreachable) as raised,
        ):
            watch.fetch_versions("x", sleep=lambda _: None)
        self.assertEqual(opened.call_count, watch.ATTEMPTS)
        self.assertIn("HTTP 500", str(raised.exception))

    def test_bad_json_is_unreachable(self) -> None:
        with (
            mock.patch("urllib.request.urlopen", return_value=FakeResponse(b"not json")),
            self.assertRaises(watch.Unreachable),
        ):
            watch.fetch_versions("x", sleep=lambda _: None)

    def test_retry_recovers(self) -> None:
        responses = [urllib.error.URLError("boom"), versions_payload(("0.1.0", False))]
        with mock.patch("urllib.request.urlopen", side_effect=responses):
            self.assertEqual(watch.fetch_versions("x", sleep=lambda _: None), [("0.1.0", False)])


class MainTests(unittest.TestCase):
    """main() exit codes and workflow-command output."""

    def run_main(self, side_effect: object) -> tuple[int, str]:
        out = io.StringIO()
        with (
            mock.patch("urllib.request.urlopen", side_effect=side_effect),
            mock.patch("time.sleep"),
            contextlib.redirect_stdout(out),
        ):
            return watch.main(), out.getvalue()

    def test_all_blocked_exits_zero(self) -> None:
        code, out = self.run_main(http_error(404))
        self.assertEqual(code, 0)
        self.assertNotIn("::error::", out)

    def test_release_exits_one(self) -> None:
        code, out = self.run_main(lambda *_a, **_k: versions_payload(("3.0.0", False)))
        self.assertEqual(code, 1)
        self.assertIn("::error::oxc_linter 3.0.0 is published", out)

    def test_unreachable_exits_two(self) -> None:
        code, out = self.run_main(http_error(500))
        self.assertEqual(code, 2)
        self.assertIn("::error::crates.io unreachable for oxc_linter: HTTP 500", out)


class GitDependencyTests(unittest.TestCase):
    """Every git dependency is either watched or a documented lockstep exception."""

    def git_dependencies(self) -> set[str]:
        found: set[str] = set()
        for manifest in (ROOT / "Cargo.toml", ROOT / "components" / "Cargo.toml"):
            if not manifest.is_file():
                continue
            data = tomllib.loads(manifest.read_text())
            tables = [data.get("dependencies", {}), data.get("workspace", {}).get("dependencies", {})]
            for table in tables:
                found.update(name for name, spec in table.items() if isinstance(spec, dict) and "git" in spec)
        return found

    def test_git_dependencies_are_watched_or_lockstep(self) -> None:
        git = self.git_dependencies()
        self.assertTrue(git, "no git dependencies parsed; the manifest scan examined nothing")
        unknown = git - set(watch.BLOCKED) - LOCKSTEP
        self.assertEqual(unknown, set(), "git dependency neither in BLOCKED nor in LOCKSTEP")


if __name__ == "__main__":
    unittest.main()
