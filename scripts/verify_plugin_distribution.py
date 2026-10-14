"""Verify product identity and exact runtime resources in registry archives."""

import email
import json
import sys
import tarfile
import zipfile
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parent.parent


def verify_resources(files: dict[str, bytes], directory: Path, names: list[str]) -> int:
    """Reject missing or changed source bytes before a registry skip can hide drift."""
    expected = [
        path
        for name in names
        for path in (directory / name).rglob("*")
        if path.is_file() and "__pycache__" not in path.parts
    ]
    if not expected:
        raise ValueError("No runtime resources examined")
    for path in expected:
        name = path.relative_to(directory).as_posix()
        if name not in files:
            raise ValueError(f"Missing bundled resource: {name}")
        if files[name] != path.read_bytes():
            raise ValueError(f"Changed bundled resource: {name}")
    return len(expected)


def verify_wheel(archive: Path) -> int:
    """Check the generated Hermes wheel against its canonical package sources."""
    directory = ROOT / "plugin/.hermes/package"
    project = tomllib.loads((directory / "pyproject.toml").read_text())["project"]
    with zipfile.ZipFile(archive) as wheel:
        files = {name: wheel.read(name) for name in wheel.namelist() if not name.endswith("/")}
    metadata = [
        email.message_from_bytes(value) for name, value in files.items() if name.endswith(".dist-info/METADATA")
    ]
    if len(metadata) != 1 or metadata[0]["Name"] != project["name"] or metadata[0]["Version"] != project["version"]:
        raise ValueError("Hermes package name or version mismatch")
    source = directory / "src"
    return verify_resources(
        files, source, [path.name for path in source.iterdir() if path.is_dir() and (path / "__init__.py").is_file()]
    )


def verify_npm(archive: Path) -> int:
    """Check Pi content and the native OpenCode adapter in the actual npm tarball."""
    directory = ROOT / "plugin"
    package = json.loads((directory / "package.json").read_text())
    with tarfile.open(archive) as tar:
        files = {
            member.name.removeprefix("package/"): tar.extractfile(member).read() for member in tar if member.isfile()
        }
    manifest = json.loads(files["package.json"])
    if manifest["name"] != package["name"] or manifest["version"] != package["version"]:
        raise ValueError("npm package name or version mismatch")
    names = [".pi/skills", ".pi/prompts", ".opencode/plugins"]
    return verify_resources(files, directory, names)


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in {"npm", "wheel"}:
        raise SystemExit("Usage: verify_plugin_distribution.py npm|wheel ARCHIVE")
    count = (verify_npm if sys.argv[1] == "npm" else verify_wheel)(Path(sys.argv[2]))
    print(f"Verified {count} exact runtime resources in {sys.argv[1]} archive")
