#!/usr/bin/env python3
"""Build the platform wheels for the ``polylint`` distribution.

    python build_wheels.py --artifacts <dir> [--out dist] [--only <triple>]
    python build_wheels.py --local <path-to-poly> [--out dist]

``--artifacts`` points at the release archives exactly as published — the same
files ``sha256sums.txt`` covers and the same files attached to the GitHub
release. Building a binary here instead would ship bytes the release checksums
do not describe, which is the one way the wheel and the release can diverge.

``--local`` stages a single locally built binary for the host platform, which is
how the packaging is exercised without a release.

Requires ``build`` and ``hatchling`` on the interpreter running this script.
"""

# poly: allow-file[T201] a build script reports progress on stdout; there is no logger here.

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from dataclasses import dataclass
from pathlib import Path

PACKAGE_DIR = Path(__file__).resolve().parent

# The earliest macOS each Rust target supports. Rust's own defaults, and what
# maturin stamps: bumping these lies to pip about what the binary will run on.
MACOS_X86_64_MIN = "10_12"
MACOS_ARM64_MIN = "11_0"

# musl 1.2 is what the rust:alpine image the musl archive is built in ships, and
# musllinux_1_2 is the tag pip matches it against (PEP 656).
MUSLLINUX_VERSION = "1_2"

# Fallback if the glibc requirement cannot be read out of the ELF. manylinux2014
# is the floor every supported aarch64 and x86_64 platform clears.
MANYLINUX_FALLBACK_MINOR = 17


@dataclass(frozen=True)
class Target:
    """One release triple and how it maps onto a wheel platform tag."""

    triple: str
    archive_suffix: str
    binary_name: str

    @property
    def archive_name_template(self) -> str:
        return f"poly-{{version}}-{self.triple}.{self.archive_suffix}"


TARGETS = (
    Target("aarch64-apple-darwin", "tar.gz", "poly"),
    Target("x86_64-apple-darwin", "tar.gz", "poly"),
    Target("aarch64-unknown-linux-gnu", "tar.gz", "poly"),
    Target("x86_64-unknown-linux-gnu", "tar.gz", "poly"),
    Target("x86_64-unknown-linux-musl", "tar.gz", "poly"),
    Target("x86_64-pc-windows-msvc", "zip", "poly.exe"),
)

GLIBC_SYMBOL = re.compile(rb"GLIBC_2\.(\d+)")


def project_version() -> str:
    """The version declared in pyproject.toml, which release-bump.sh keeps in lock-step."""
    text = (PACKAGE_DIR / "pyproject.toml").read_text(encoding="utf-8")
    match = re.search(r'^version = "([^"]+)"$', text, re.MULTILINE)
    if match is None:
        message = "pyproject.toml has no [project] version"
        raise SystemExit(message)
    return match.group(1)


def required_glibc_minor(binary: Path) -> int:
    """Highest ``GLIBC_2.x`` symbol version the binary references.

    Read from the binary rather than hardcoded from the builder image: the
    runner image's glibc moves on its own schedule, and a tag claiming an older
    glibc than the binary actually needs produces a wheel that installs happily
    and then dies on ``exec`` with a symbol-lookup error.
    """
    versions = [int(match) for match in GLIBC_SYMBOL.findall(binary.read_bytes())]
    return max(versions) if versions else MANYLINUX_FALLBACK_MINOR


def platform_tag(target: Target, binary: Path) -> str:
    """The PEP 425 platform tag for a target's wheel."""
    if target.triple == "x86_64-apple-darwin":
        return f"macosx_{MACOS_X86_64_MIN}_x86_64"
    if target.triple == "aarch64-apple-darwin":
        return f"macosx_{MACOS_ARM64_MIN}_arm64"
    if target.triple == "x86_64-pc-windows-msvc":
        return "win_amd64"
    if target.triple == "x86_64-unknown-linux-musl":
        return f"musllinux_{MUSLLINUX_VERSION}_x86_64"
    machine = "aarch64" if target.triple.startswith("aarch64") else "x86_64"
    return f"manylinux_2_{required_glibc_minor(binary)}_{machine}"


def extract_binary(archive: Path, target: Target, destination: Path) -> Path:
    """Unpack the single binary out of a published release archive."""
    destination.mkdir(parents=True, exist_ok=True)
    output = destination / target.binary_name

    if target.archive_suffix == "zip":
        with (
            zipfile.ZipFile(archive) as zf,
            zf.open(target.binary_name) as source,
            output.open("wb") as sink,
        ):
            shutil.copyfileobj(source, sink)
    else:
        with tarfile.open(archive, "r:gz") as tf:
            member = tf.extractfile(target.binary_name)
            if member is None:
                message = f"{archive} contains no {target.binary_name}"
                raise SystemExit(message)
            with output.open("wb") as sink:
                shutil.copyfileobj(member, sink)

    output.chmod(0o755)
    return output


def force_executable_bit(wheel: Path, member_prefix: str = "polylint/bin/") -> None:
    """Rewrite the wheel so the bundled binary unpacks with mode 0755.

    Wheel entries carry their mode in the zip's external attributes and pip
    honours it. Setting it here rather than trusting the build backend to
    preserve the source file's mode means a wheel can never install a binary
    that is not executable — a failure that only shows up at first run.

    RECORD hashes file *contents*, which are untouched, so the wheel stays
    internally consistent.
    """
    rewritten = wheel.with_suffix(".whl.tmp")
    with zipfile.ZipFile(wheel) as source, zipfile.ZipFile(rewritten, "w", zipfile.ZIP_DEFLATED) as sink:
        for info in source.infolist():
            data = source.read(info.filename)
            new_info = zipfile.ZipInfo(info.filename, date_time=info.date_time)
            new_info.compress_type = info.compress_type
            new_info.external_attr = info.external_attr
            if info.filename.startswith(member_prefix):
                # 0o100755: regular file, rwxr-xr-x. The S_IFREG bits matter — some
                # unpackers reject an entry whose mode names no file type.
                new_info.external_attr = (0o100755 << 16) | (info.external_attr & 0xFFFF)
            sink.writestr(new_info, data)
    rewritten.replace(wheel)


def run_build(out_dir: Path, env_overrides: dict[str, str]) -> Path:
    """Invoke the wheel backend once and return the wheel it produced."""
    before = set(out_dir.glob("*.whl"))
    # Start from a copy with both hook variables cleared, so a stale value in the
    # ambient environment cannot turn the fallback wheel into a platform one.
    env = dict(os.environ)
    env.pop("POLY_WHEEL_TAG", None)
    env.pop("POLY_WHEEL_BINARY", None)
    env.update(env_overrides)

    subprocess.run(  # noqa: S603 - fixed argv
        [sys.executable, "-m", "build", "--wheel", "--no-isolation", "--outdir", str(out_dir)],
        cwd=PACKAGE_DIR,
        check=True,
        env=env,
    )

    produced = set(out_dir.glob("*.whl")) - before
    if len(produced) != 1:
        message = f"expected exactly one new wheel, got {sorted(p.name for p in produced)}"
        raise SystemExit(message)
    return produced.pop()


def build_platform_wheel(target: Target, binary: Path, out_dir: Path) -> Path:
    tag = platform_tag(target, binary)
    wheel = run_build(out_dir, {"POLY_WHEEL_TAG": tag, "POLY_WHEEL_BINARY": str(binary)})
    force_executable_bit(wheel)
    print(f"  ✓ {target.triple:32} → {wheel.name}")
    return wheel


def build_fallback_wheel(out_dir: Path) -> Path:
    """The ``py3-none-any`` wheel an unsupported platform resolves to.

    pip ranks platform-specific wheels above ``any``, so this is only ever
    chosen when nothing else matches — and then it gives the user a ``poly``
    command that explains which platforms are published, instead of pip's bare
    "could not find a version that satisfies the requirement".
    """
    wheel = run_build(out_dir, {})
    print(f"  ✓ {'fallback (no binary)':32} → {wheel.name}")
    return wheel


def host_target() -> Target:
    import platform

    machine = platform.machine()
    system = platform.system()
    if system == "Darwin":
        triple = "aarch64-apple-darwin" if machine == "arm64" else "x86_64-apple-darwin"
    elif system == "Linux":
        triple = "aarch64-unknown-linux-gnu" if machine == "aarch64" else "x86_64-unknown-linux-gnu"
    else:
        message = f"--local has no target mapping for {system}/{machine}"
        raise SystemExit(message)
    return next(target for target in TARGETS if target.triple == triple)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--artifacts", type=Path, help="directory holding the published release archives")
    source.add_argument("--local", type=Path, help="a locally built poly binary (host platform only)")
    parser.add_argument("--out", type=Path, default=PACKAGE_DIR / "dist", help="output directory")
    parser.add_argument("--only", help="build just this triple (or 'fallback')")
    parser.add_argument("--no-fallback", action="store_true", help="skip the py3-none-any wheel")
    args = parser.parse_args()

    out_dir = args.out.resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    version = project_version()
    print(f"Building polylint {version} wheels into {out_dir}")

    with tempfile.TemporaryDirectory() as staging_root:
        staging = Path(staging_root)

        if args.local is not None:
            target = host_target()
            binary = staging / target.triple / target.binary_name
            binary.parent.mkdir(parents=True)
            shutil.copy2(args.local, binary)
            binary.chmod(0o755)
            build_platform_wheel(target, binary, out_dir)
        else:
            artifacts = args.artifacts.resolve()
            for target in TARGETS:
                if args.only and args.only != target.triple:
                    continue
                archive = artifacts / target.archive_name_template.format(version=version)
                if not archive.is_file():
                    message = f"missing release archive: {archive}"
                    raise SystemExit(message)
                binary = extract_binary(archive, target, staging / target.triple)
                build_platform_wheel(target, binary, out_dir)

        if not args.no_fallback and (args.only in (None, "fallback")):
            build_fallback_wheel(out_dir)

    print(f"\nWheels in {out_dir}:")
    for wheel in sorted(out_dir.glob("*.whl")):
        print(f"  {wheel.name}")


if __name__ == "__main__":
    main()
