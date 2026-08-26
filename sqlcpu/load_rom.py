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

**Density invariant (issue #81).** After this script runs, `ram` holds exactly
one row for every word in SPEC §2's 24 MiB region — the image, then explicit
zeros for the rest. That is not padding for tidiness: both readers index the
captured RAM array *positionally* (`executor/fold.py`'s `RAMT`, and
`sqlcpu/run_riscv_tests.py`'s `RAM_T[wa - RAM_BASE_WORD + 1]`), and
`groupArray(...) ORDER BY word_addr` yields the i-th *populated* word, not the
word at index i. Those two coincide only while `ram` is dense from RAM_BASE.

Loading the image alone leaves it dense by accident, and DOOM breaks the
accident ~187k instructions into boot: `sp` starts at 0x81800000 (top of RAM),
so the first stack push lands ~5M words above BSS and opens a hole. Measured on
the real ROM: 1,258,766 rows across a 6,291,456-word span, after which every
load past the hole silently reads the wrong word — no error, no halt, and
perfectly reproducible, so SPEC §8 determinism is satisfied while the answer is
wrong.

Zero-filling costs 0.2s once and 27 MiB on disk (zeros compress), and moves the
fold from 1,128 to 1,116 instr/sec on the real ROM — within noise. Correct by
construction beats correct by coincidence at that price.
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
        "--ram-words",
        type=int,
        default=6_291_456,  # SPEC §2: 24 MiB / 4
        help="size of SPEC §2's RAM region in words; the span zero-filled above "
             "the image to keep `ram` dense (default: %(default)s)",
    )
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

    base_cmd = args.client.split() + [
        "--host", args.host,
        "--port", str(args.port),
        "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    client_cmd = base_cmd + ["--query", "INSERT INTO ram (word_addr, value, version) FORMAT TSV"]

    result = subprocess.run(client_cmd, input=rows, text=True)
    if result.returncode != 0:
        return result.returncode

    print(f"loaded {len(words)} words ({len(blob)} bytes) at word_addr {base_word}.."
          f"{base_word + len(words) - 1} (byte {load_addr:#x}..{load_addr + len(blob) - 1:#x})",
          file=sys.stderr)

    # Zero-fill the rest of SPEC §2's RAM region, so `ram` is dense over
    # [base_word, base_word + ram_words) -- see the density invariant in this
    # module's docstring (#81). Generated server-side from numbers(): 5M rows
    # over the wire would dwarf the ROM load itself, and this is the same
    # "computes nothing about what a word means" housekeeping PURITY.md action
    # 4 allows -- a constant zero at an arithmetic address, no opcode inspected.
    #
    # version 0 matches the image rows above. There is deliberately no overlap
    # to tie-break: the fill starts one word past the image's last word, so no
    # word_addr gets two rows at the same version.
    fill_start = base_word + len(words)
    fill_end = base_word + args.ram_words  # exclusive
    if fill_start < fill_end:
        fill_cmd = base_cmd + [
            "--query",
            "INSERT INTO ram (word_addr, value, version) "
            f"SELECT toUInt32({fill_start} + number), toUInt32(0), toUInt64(0) "
            f"FROM numbers({fill_end - fill_start})",
        ]
        fill = subprocess.run(fill_cmd, text=True)
        if fill.returncode != 0:
            return fill.returncode
        print(f"zero-filled {fill_end - fill_start} words to word_addr {fill_end - 1} "
              f"(ram now dense over SPEC §2's {args.ram_words}-word region)", file=sys.stderr)
    elif fill_start > fill_end:
        print(f"::error::image ends at word_addr {fill_start - 1}, past the "
              f"{args.ram_words}-word RAM region ending at {fill_end - 1}", file=sys.stderr)
        return 1

    # Verify the invariant rather than assume it. Cheap (one aggregate over a
    # sorted key) and it fails at load time, where the cause is obvious --
    # unlike the corruption it prevents, which surfaces as a wrong register
    # value a hundred thousand instructions later with no error anywhere.
    check_cmd = base_cmd + [
        "--query",
        "SELECT count(), max(word_addr) - min(word_addr) + 1, min(word_addr) FROM ram FINAL",
    ]
    check = subprocess.run(check_cmd, text=True, capture_output=True)
    if check.returncode != 0:
        print(check.stderr, file=sys.stderr, end="")
        return check.returncode
    rows, span, lowest = (int(x) for x in check.stdout.split())
    if rows != span or rows != args.ram_words or lowest != base_word:
        print(
            f"::error::ram is not dense over the RAM region: {rows} rows spanning "
            f"{span} words from {lowest}, expected {args.ram_words} rows spanning "
            f"{args.ram_words} from {base_word}. RAMT indexes positionally -- see #81.",
            file=sys.stderr,
        )
        return 1
    print(f"ram dense: {rows} rows, word_addr {lowest}..{lowest + rows - 1}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    sys.exit(main())
