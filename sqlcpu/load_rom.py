#!/usr/bin/env python3
"""Load a ROM image into clickdoom.ram — sqlcpu workstream, issue #18.

PURITY.md, action 4: "the driver may load the ROM bytes... it may not decode
them." This script does exactly and only that: it reinterprets the flat
binary's bytes as little-endian 32-bit words and inserts them at their word
address. It never inspects an opcode field, never branches on instruction
content, and never produces a value that depends on what a word *means* as
RISC-V — only on where its bytes sit. Decoding (sqlcpu/decode.sql) is a
separate, later, purely-SQL step over the rows this script writes.

Usage:
    load_rom.py --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --host localhost --port 9000 --password clickdoom

manifest.json fields, per SPEC §4: spec_version, entry, load_addr, size,
sha256, text_start, text_end. `size` and `sha256` are checked against the
actual file before anything is loaded — a mismatch means the ROM artifact is
corrupt or stale, not something to silently load anyway.
"""
import argparse
import hashlib
import json
import struct
import subprocess
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin", required=True, help="path to the flat ROM binary")
    ap.add_argument("--manifest", required=True, help="path to manifest.json")
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", default="clickdoom")
    ap.add_argument(
        "--client",
        default="clickhouse-client",
        help="client binary; pass 'clickhouse client' (two words, quoted) for the standalone build",
    )
    args = ap.parse_args()

    with open(args.manifest) as f:
        manifest = json.load(f)

    with open(args.bin, "rb") as f:
        blob = f.read()

    if manifest.get("size") is not None and len(blob) != manifest["size"]:
        print(
            f"::error::{args.bin}: size {len(blob)} != manifest size {manifest['size']}",
            file=sys.stderr,
        )
        return 1

    digest = hashlib.sha256(blob).hexdigest()
    if manifest.get("sha256") and digest != manifest["sha256"]:
        print(
            f"::error::{args.bin}: sha256 {digest} != manifest sha256 {manifest['sha256']}",
            file=sys.stderr,
        )
        return 1

    if len(blob) % 4 != 0:
        print(f"::error::{args.bin}: length {len(blob)} is not a multiple of 4", file=sys.stderr)
        return 1

    load_addr = manifest["load_addr"]
    if load_addr % 4 != 0:
        print(f"::error::manifest load_addr {load_addr:#x} is not word-aligned", file=sys.stderr)
        return 1

    base_word = load_addr >> 2
    words = struct.unpack(f"<{len(blob) // 4}I", blob)

    # TSV to stdin, not one INSERT per word: this is the only way to load a
    # multi-megabyte ROM in one client round trip rather than millions.
    rows = "\n".join(f"{base_word + i}\t{w}\t0" for i, w in enumerate(words))

    client_cmd = args.client.split() + [
        "--host", args.host,
        "--port", str(args.port),
        "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        client_cmd += ["--password", args.password]
    client_cmd += ["--query", "INSERT INTO ram (word_addr, value, version) FORMAT TSV"]

    result = subprocess.run(client_cmd, input=rows, text=True)
    if result.returncode != 0:
        return result.returncode

    print(f"loaded {len(words)} words ({len(blob)} bytes) at word_addr {base_word}.."
          f"{base_word + len(words) - 1} (byte {load_addr:#x}..{load_addr + len(blob) - 1:#x})",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
