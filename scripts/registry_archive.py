"""Read bounded package payloads without accepting ambiguous archive inventories."""

import base64
import csv
import email
import hashlib
import io
import json
import stat
import tarfile
import zipfile
from pathlib import PurePosixPath
from typing import BinaryIO, NoReturn

Identity = tuple[str, str]
Payload = dict[str, tuple[int, bytes]]
JsonObject = dict[str, object]

MAX_MEMBERS = 10_000
MAX_MEMBER_BYTES = 256 * 1024 * 1024
MAX_PAYLOAD_BYTES = 512 * 1024 * 1024


def unique_object(pairs: list[tuple[str, object]]) -> JsonObject:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"Duplicate JSON key: {key}")
        result[key] = value
    return result


def invalid_constant(value: str) -> NoReturn:
    raise ValueError(f"Invalid JSON constant: {value}")


def json_object(data: bytes | str) -> JsonObject:
    value = json.loads(data, object_pairs_hook=unique_object, parse_constant=invalid_constant)
    if not isinstance(value, dict):
        raise TypeError("Expected a JSON object")
    return value


def safe_name(name: str) -> str:
    path = PurePosixPath(name)
    if not name or path.is_absolute() or ".." in path.parts or "\\" in name or str(path) != name:
        raise ValueError(f"Unsafe archive member: {name}")
    return name


def check_inventory(sizes: list[int]) -> None:
    if not sizes or len(sizes) > MAX_MEMBERS:
        raise ValueError("Empty or excessive archive inventory")
    if any(size < 0 or size > MAX_MEMBER_BYTES for size in sizes) or sum(sizes) > MAX_PAYLOAD_BYTES:
        raise ValueError("Archive payload exceeds size limits")


def insert_member(files: Payload, name: str, mode: int, stream: BinaryIO, size: int) -> None:
    safe_name(name)
    if name in files:
        raise ValueError(f"Duplicate archive member: {name}")
    data = stream.read(size + 1)
    if len(data) != size:
        raise ValueError(f"Invalid archive member size: {name}")
    files[name] = (mode & 0o7777, data)


def npm_payload(data: bytes) -> tuple[Identity, Payload]:
    files = {}
    try:
        with tarfile.open(fileobj=io.BytesIO(data)) as archive:
            members = [member for member in archive if not member.isdir()]
            check_inventory([member.size for member in members])
            for member in members:
                if not member.isfile() or not member.name.startswith("package/"):
                    raise ValueError("npm archive must contain regular files under package/")
                insert_member(files, member.name[8:], member.mode, archive.extractfile(member), member.size)
    except tarfile.TarError as error:
        raise ValueError("Malformed npm archive") from error
    if "package.json" not in files:
        raise ValueError("Missing npm package manifest")
    mode, manifest = files["package.json"]
    manifest = json_object(manifest)
    identity = package_identity(manifest)
    files["package.json"] = (mode, json.dumps(manifest, sort_keys=True).encode())
    return identity, files


def package_identity(manifest: JsonObject) -> Identity:
    name, version = manifest.get("name"), manifest.get("version")
    if not isinstance(name, str) or not isinstance(version, str):
        raise TypeError("Missing package name or version")
    if not name.strip() or not version.strip():
        raise ValueError("Empty package name or version")
    return name, version


def validate_record(files: Payload) -> str:
    records = [name for name in files if name.endswith(".dist-info/RECORD")]
    if len(records) != 1:
        raise ValueError("Expected exactly one wheel RECORD")
    name = records[0]
    rows = list(csv.reader(io.StringIO(files[name][1].decode())))
    indexed = {}
    for row in rows:
        if len(row) != 3 or row[0] in indexed:
            raise ValueError("Invalid or duplicate wheel RECORD row")
        indexed[row[0]] = row
    if set(indexed) != set(files):
        raise ValueError("Wheel RECORD inventory mismatch")
    for path, (_, data) in files.items():
        _, digest, size = indexed[path]
        if path == name:
            if digest or size:
                raise ValueError("Wheel RECORD must not hash itself")
            continue
        expected = "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("=")
        if digest != expected or size != str(len(data)):
            raise ValueError(f"Invalid wheel RECORD digest or size: {path}")
    files[name] = (files[name][0], json.dumps(sorted(rows)).encode())
    return name


def wheel_payload(data: bytes) -> tuple[Identity, Payload]:
    files = {}
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            members = [member for member in archive.infolist() if not member.is_dir()]
            check_inventory([member.file_size for member in members])
            for member in members:
                mode = member.external_attr >> 16
                if stat.S_ISLNK(mode):
                    raise ValueError("Wheel symlinks are not supported")
                with archive.open(member) as stream:
                    insert_member(files, member.filename, mode, stream, member.file_size)
    except zipfile.BadZipFile as error:
        raise ValueError("Malformed wheel archive") from error
    record = validate_record(files)
    metadata = [name for name in files if name.endswith(".dist-info/METADATA")]
    if len(metadata) != 1 or metadata[0].rsplit("/", 1)[0] != record.rsplit("/", 1)[0]:
        raise ValueError("Expected one matching wheel metadata directory")
    message = email.message_from_bytes(files[metadata[0]][1])
    if len(message.get_all("Name", [])) != 1 or len(message.get_all("Version", [])) != 1:
        raise ValueError("Missing or duplicate wheel identity metadata")
    return package_identity({"name": message["Name"], "version": message["Version"]}), files


def compare_payloads(candidate: Payload, published: Payload) -> None:
    if candidate != published:
        changed = next(
            name for name in sorted(candidate.keys() | published.keys()) if candidate.get(name) != published.get(name)
        )
        raise ValueError(f"Immutable payload conflict: {changed}; select a fresh version")
