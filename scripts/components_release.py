#!/usr/bin/env python3
"""Release-side helpers for the downloadable engine components.

    components_release.py list --target <triple>
    components_release.py count-targets
    components_release.py triples
    components_release.py input-hash --component <name> --target <triple> [--extra KEY=VALUE ...]

`list` prints "<component><TAB><producer manifest path>" per component alef.toml builds for a triple. `input-hash` prints the content
address of one component build: everything that can change the bytes of the built library.
publish.yaml compares it with the hash recorded next to the previous release's asset and reuses
that asset when they are equal.

The hash covers, for the producer crate's dependency closure on the target:
  * every registry or git package, by name, version, source and Cargo.lock checksum;
  * every workspace path package (the engine crates, the protocol and the api), by the git blob
    hashes of its tracked files, so a source edit changes it and a workspace version bump does not;
  * the resolved feature set of each of those packages (`cargo metadata` resolve.nodes[].features);
  * the root Cargo.toml `[profile]`, `[workspace.dependencies]` and `[patch]` tables, and the
    contents of rust-toolchain.toml and .cargo/config.toml when present;
  * the contract hashes, the component's feature set, the alef.toml component distribution
    (URL templates and public keys) and the target;
  * the caller's --extra values: the pinned rust toolchain, alef version, signing key id, RUSTFLAGS
    and the container image.
"""

# poly: allow-file[T201] a release script reports on stdout; there is no logger here.

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HASH_VERSION = "1"


def load_alef() -> dict:
    return tomllib.loads((ROOT / "alef.toml").read_text(encoding="utf-8"))


def component_profiles(alef: dict) -> list[dict]:
    return [component for crate in alef["crates"] for component in crate.get("components", [])]


def producer_package(alef: dict, component: str) -> str:
    """The producer crate's package name, following alef's `<core crate dir>-<component>-component`."""
    for crate in alef["crates"]:
        if any(profile["name"] == component for profile in crate.get("components", [])):
            return f"{crate['name']}-{component.replace('_', '-')}-component"
    message = f"no component named {component!r} in alef.toml"
    raise SystemExit(message)


def cargo_metadata(target: str) -> dict:
    output = subprocess.run(  # noqa: S603 - fixed argv
        ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target],  # noqa: S607
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(output)


def closure(metadata: dict, root_name: str) -> list[dict]:
    """The packages reachable from `root_name` through normal and build dependencies."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [package["id"] for package in packages.values() if package["name"] == root_name]
    if len(roots) != 1:
        message = f"expected exactly one package named {root_name!r}, found {len(roots)}"
        raise SystemExit(message)
    seen: set[str] = set()
    stack = roots
    while stack:
        current = stack.pop()
        if current in seen:
            continue
        seen.add(current)
        for dep in nodes[current]["deps"]:
            kinds = {kind["kind"] for kind in dep["dep_kinds"]}
            if kinds & {None, "build"}:
                stack.append(dep["pkg"])
    return sorted((packages[package_id] for package_id in seen), key=lambda package: package["id"])


def lock_checksums() -> dict[tuple[str, str, str], str]:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text(encoding="utf-8"))
    return {
        (package["name"], package["version"], package.get("source", "")): package.get("checksum", "")
        for package in lock["package"]
    }


def tracked_tree_digest(directory: Path) -> str:
    """Digest of the git blob ids of the tracked files under `directory`, relative to it."""
    listing = subprocess.run(  # noqa: S603 - fixed argv
        ["git", "ls-files", "-s", "-z", "--", str(directory)],  # noqa: S607
        cwd=ROOT,
        check=True,
        capture_output=True,
    ).stdout
    entries = sorted(entry for entry in listing.split(b"\0") if entry)
    if not entries:
        message = f"no tracked files under {directory}"
        raise SystemExit(message)
    prefix = f"{directory.relative_to(ROOT).as_posix()}/".encode()
    digest = hashlib.sha256()
    for entry in entries:
        meta, _, path = entry.partition(b"\t")
        digest.update(meta.split()[1])
        digest.update(path.removeprefix(prefix))
        digest.update(b"\n")
    return digest.hexdigest()


def manifest_tables() -> str:
    """The root manifest tables that change how any dependency builds."""
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    tables = {
        "profile": cargo.get("profile", {}),
        "workspace.dependencies": cargo.get("workspace", {}).get("dependencies", {}),
        "patch": cargo.get("patch", {}),
    }
    return json.dumps(tables, sort_keys=True)


def build_config_files() -> list[str]:
    """Digests of the files cargo and rustup read implicitly; absent files are recorded as absent."""
    parts = []
    for name in ("rust-toolchain.toml", ".cargo/config.toml"):
        path = ROOT / name
        digest = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else "absent"
        parts.append(f"file:{name}={digest}")
    return parts


def distribution(alef: dict) -> str:
    """The URL templates and public keys the embedded lock is built from."""
    return json.dumps(
        [crate.get("component_distribution", {}) for crate in alef["crates"]],
        sort_keys=True,
    )


def contract_hashes() -> str:
    """The contract hashes alef records in the lock; read from the generated host module."""
    module = (ROOT / "crates/poly-component-host/src/components_gen.rs").read_text(encoding="utf-8")
    return hashlib.sha256(module.encode()).hexdigest()


def input_hash(component: str, target: str, extras: list[str], metadata: dict | None = None) -> str:
    alef = load_alef()
    profile = next((p for p in component_profiles(alef) if p["name"] == component), None)
    if profile is None:
        message = f"no component named {component!r} in alef.toml"
        raise SystemExit(message)
    metadata = metadata if metadata is not None else cargo_metadata(target)
    checksums = lock_checksums()
    parts = [
        f"hash-version={HASH_VERSION}",
        f"component={component}",
        f"target={target}",
        f"features={','.join(sorted(profile.get('features', [])))}",
        f"default-features={profile.get('default_features', True)}",
        f"manifest={manifest_tables()}",
        f"distribution={distribution(alef)}",
        f"contracts={contract_hashes()}",
    ]
    parts.extend(build_config_files())
    parts.extend(f"extra:{extra}" for extra in sorted(extras))
    features = {node["id"]: node.get("features", []) for node in metadata["resolve"]["nodes"]}
    for package in closure(metadata, producer_package(alef, component)):
        # A workspace package's version is left out on purpose: a lock-step bump must not rebuild.
        version = "" if package["source"] is None else package["version"]
        parts.append(f"features:{package['name']}@{version}={','.join(sorted(features[package['id']]))}")
        if package["source"] is None:
            tree = tracked_tree_digest(Path(package["manifest_path"]).parent)
            parts.append(f"path:{package['name']}={tree}")
        else:
            checksum = checksums.get((package["name"], package["version"], package["source"]), "")
            parts.append(f"dep:{package['name']}@{package['version']}#{package['source']}#{checksum}")
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    listing = sub.add_parser("list", help="components alef.toml builds for a target")
    listing.add_argument("--target", required=True)
    sub.add_parser("triples", help="every target triple any component builds for, one per line")
    sub.add_parser("count-targets", help="number of (component, target) pairs alef.toml configures")
    digest = sub.add_parser("input-hash", help="content address of one component build")
    digest.add_argument("--component", required=True)
    digest.add_argument("--target", required=True)
    digest.add_argument("--extra", action="append", default=[], help="KEY=VALUE folded into the hash")
    digest.add_argument("--metadata-file", type=Path, help="a saved `cargo metadata` document (tests)")
    args = parser.parse_args()

    if args.command == "list":
        alef = load_alef()
        for profile in component_profiles(alef):
            if args.target in profile.get("targets", []):
                print(f"{profile['name']}\tcrates/{producer_package(alef, profile['name'])}/Cargo.toml")
        return
    if args.command == "triples":
        print("\n".join(sorted({t for profile in component_profiles(load_alef()) for t in profile.get("targets", [])})))
        return
    if args.command == "count-targets":
        print(sum(len(profile.get("targets", [])) for profile in component_profiles(load_alef())))
        return
    metadata = json.loads(args.metadata_file.read_text(encoding="utf-8")) if args.metadata_file else None
    print(input_hash(args.component, args.target, args.extra, metadata))


if __name__ == "__main__":
    sys.exit(main())
