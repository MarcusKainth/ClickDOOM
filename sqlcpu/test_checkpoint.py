#!/usr/bin/env python3
"""Correctness test for sqlcpu/checkpoint.py — issue #22.

Re-verifies refemu's exact worked examples (issue #15's comment thread,
refemu/tests/test_trace.py) against checkpoint.py's generated SQL, run for
real against ClickHouse — not re-pasted numbers. Byte-identical output
against refemu is the entire point of this file (SPEC §7); a passing test
here is direct evidence of that, not a proxy for it.

Usage:
    test_checkpoint.py --host localhost --port 9000 --password clickdoom
"""
import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import checkpoint as cp  # noqa: E402


def build_query() -> str:
    checks = []

    def check(name, expr, expected):
        checks.append((name, f"({expr}) = {expected!r}" if isinstance(expected, str)
                        else f"({expr}) = {expected}"))

    # reg_hash: refemu's two worked examples (issue #15).
    regs_zero = "[" + ",".join(["0"] * 31) + "]"
    check(
        "reg_hash all-zero regs, pc=0x80000004",
        cp.reg_hash(pc="toUInt32(2147483652)", regs=regs_zero),
        4903144380889844081,
    )
    regs_mixed = ["3735928559" if i == 1 else "42" if i == 10 else
                  "4294967295" if i == 31 else "0" for i in range(1, 32)]
    check(
        "reg_hash x1=0xdeadbeef x10=42 x31=0xffffffff, pc=0x80000100",
        cp.reg_hash(pc="toUInt32(2147483904)", regs="[" + ",".join(regs_mixed) + "]"),
        11036197505622382625,
    )

    # word_array_hash (ram_hash): 16 words whose LE byte serialization is
    # exactly bytes 0x00..0x3f (refemu's ram_hash worked example, #15).
    words16 = [50462976, 117835012, 185207048, 252579084, 319951120, 387323156,
               454695192, 522067228, 589439264, 656811300, 724183336, 791555372,
               858927408, 926299444, 993671480, 1061043516]
    check(
        "word_array_hash: 16 words == bytes 0x00..0x3f",
        cp.word_array_hash("[" + ",".join(str(w) for w in words16) + "]"),
        17854084224570037232,
    )

    # fb_hash: refemu's test_fb_hash_matches_clickhouse (issue #55/#56).
    check(
        "fb_hash: fb=bytes(range(16)), palette=bytes(range(200,208))",
        cp.fb_hash(
            framebuffer="unhex('000102030405060708090a0b0c0d0e0f')",
            palette="unhex('c8c9cacbcccdcecf')",
        ),
        10814741248291066246,
    )

    # hex formatting.
    check("hex64 zero-padding", cp.hex64("toUInt64(255)"), "00000000000000ff")
    check("hex32 zero-padding", cp.hex32("toUInt32(255)"), "000000ff")

    # format_checkpoint line shape.
    check(
        "format_checkpoint, plain (no ramhash/fbhash)",
        cp.format_checkpoint(icount="toUInt64(4096)", pc="toUInt32(2147487744)",
                              reghash="toUInt64(1311768467463790320)"),
        "4096\t80001000\t123456789abcdef0",
    )
    check(
        "format_checkpoint, with ramhash+fbhash",
        cp.format_checkpoint(icount="toUInt64(1048576)", pc="toUInt32(2147483648)",
                              reghash="toUInt64(0)", ramhash="toUInt64(1)", fbhash="toUInt64(2)"),
        "1048576\t80000000\t0000000000000000\t0000000000000001\t0000000000000002",
    )

    # checkpoint cadence.
    check("is_checkpoint true at a boundary", cp.is_checkpoint(icount="toUInt64(8192)"), 1)
    check("is_checkpoint false off a boundary", cp.is_checkpoint(icount="toUInt64(8193)"), 0)
    check("is_ram_hash_checkpoint true at 1,048,576", cp.is_ram_hash_checkpoint(icount="toUInt64(1048576)"), 1)
    check("is_ram_hash_checkpoint false at 4,096 (checkpoint but not ram-hash)",
          cp.is_ram_hash_checkpoint(icount="toUInt64(4096)"), 0)

    parts = [f"SELECT '{name}' AS name, ({cond}) AS ok" for name, cond in checks]
    return "\nUNION ALL\n".join(parts) + "\nFORMAT TSVWithNames"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--client", default="clickhouse-client")
    args = ap.parse_args()

    client_cmd = args.client.split() + ["--host", args.host, "--port", str(args.port), "--user", args.user]
    if args.password:
        client_cmd += ["--password", args.password]

    query = build_query()
    result = subprocess.run(client_cmd, input=query, text=True, capture_output=True)
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        return 1

    lines = result.stdout.strip("\n").split("\n")
    header, data = lines[0].split("\t"), lines[1:]
    fail = 0
    for line in data:
        cols = dict(zip(header, line.split("\t")))
        if cols["ok"] != "1":
            fail += 1
            print(f"::error::checkpoint check failed: {cols['name']}", file=sys.stderr)

    total = len(data)
    if fail:
        print(f"checkpoint.py: {total - fail}/{total} checks passed, {fail} FAILED", file=sys.stderr)
        return 1
    print(f"checkpoint.py: all {total} checks passed (refemu's exact worked examples, issues #15/#55/#56)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
