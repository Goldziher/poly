#!/usr/bin/env python3
# A stand-in sidecar speaking alef's framed stdio protocol. It answers poly's protocol calls with
# canned bytes (hex, substituted below) and misbehaves on one chosen call, so the host's handling of
# a crash, a hang, a protocol mismatch and a describe mismatch is tested without a real component.
import json
import os
import struct
import sys
import time

SCENARIO = "__SCENARIO__"
INIT = bytes.fromhex("__INIT__")
OK = bytes.fromhex("__OK__")
DESCRIBE = bytes.fromhex("__DESCRIBE__")
FORMATTED = bytes.fromhex("__FORMATTED__")

here = os.path.dirname(os.path.realpath(sys.argv[0]))
manifest = json.load(open(os.path.join(here, "component.json")))
descriptor = {
    "protocol_version": 1,
    "abi_major": 1,
    "abi_minor": 0,
    "component_id": manifest["identity"]["component"],
    "component_version": manifest["identity"]["version"],
    "feature_set_hash": manifest["identity"]["feature_hash"],
    "provides": manifest["provides"],
}


def read_frame():
    head = sys.stdin.buffer.read(4)
    if len(head) < 4:
        sys.exit(0)
    return json.loads(sys.stdin.buffer.read(struct.unpack("<I", head)[0]))


def write_frame(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(struct.pack("<I", len(body)) + body)
    sys.stdout.buffer.flush()


def reply(payload, status=0):
    write_frame({"status": status, "payload": list(payload)})


calls = 0
while True:
    request = read_frame()
    if request["op"] == "describe":
        reply(json.dumps(descriptor).encode())
        continue
    method = request["method"]
    if method == "describe":
        reply(DESCRIBE)
        continue
    calls += 1
    # call 1 is Init, 2 RegisterConfig, 3 the first Format
    if SCENARIO == "crash_at_3" and calls == 3:
        os._exit(7)
    if SCENARIO == "hang_at_3" and calls == 3:
        time.sleep(3600)
    if SCENARIO == "status_error_at_3" and calls == 3:
        reply(b"producer failed", status=3)
        continue
    if SCENARIO == "garbage_at_3" and calls == 3:
        reply(b"\xc1not messagepack")
        continue
    reply({1: INIT, 2: OK}.get(calls, FORMATTED))
