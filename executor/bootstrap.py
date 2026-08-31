#!/usr/bin/env python3
"""Seed `batch_commit`'s batch_id=0 row -- SPEC §1's reset state (`pc =
0x8000_0000`, `x1..x31 = 0`), so the first real batch's `PREV` lookup
(`fold.py`'s `batch()`, `ORDER BY batch_id DESC LIMIT 1`) has a row to read.
A fixed-literal insert with no computation -- PURITY.md action 4
("housekeeping that computes nothing"), same category as the ROM loader
loading ROM bytes: this establishes batch-execution state (SPEC §6), not
ROM/memory state (SPEC §4), so it's its own script rather than folded into
`clickdoom load-rom`.

Run once, before the driver's first batch. Idempotent in the sense that
re-running it is harmless *before* any real batch has committed (batch_id=0
stays the reset state either way); it is NOT meant to be re-run after
batch_id=0 has been superseded -- there is nothing to recover here, unlike
`commit.py`'s flushes, since this writes exactly one row, once, ever, per
fresh run.

Usage:
    bootstrap.py --host localhost --port 9000 --password clickdoom
"""
import argparse
import subprocess
import sys

import config


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", default="clickdoom")
    ap.add_argument("--client", default="clickhouse-client",
                     help="client binary; pass 'clickhouse client' (two words, quoted) "
                          "for the standalone build")
    ap.add_argument("--regs", type=int, nargs=31, default=[0] * 31,
                     help="override the 31-element x1..x31 reset vector (default: all zero, SPEC §1)")
    args = ap.parse_args()

    base_cmd = args.client.split() + [
        "--host", args.host,
        "--port", str(args.port),
        "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    existing = subprocess.run(
        base_cmd + ["--query", "SELECT count() FROM batch_commit WHERE batch_id = 0"],
        capture_output=True, text=True,
    )
    if existing.returncode != 0:
        print(existing.stderr, file=sys.stderr, end="")
        return existing.returncode
    if int(existing.stdout.strip() or "0") > 0:
        print("batch_commit already has a batch_id=0 row -- not seeding again "
              "(this is a fresh-run bootstrap, not a recovery step)", file=sys.stderr)
        return 0

    regs = "[" + ",".join(str(r) for r in args.regs) + "]"
    insert = base_cmd + [
        "--query",
        "INSERT INTO batch_commit "
        "(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
        " keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
        f"VALUES (0, 0, {config.RAM_BASE}, {regs}, 0, '', 0, "
        f" 0, 0, 0, [], [], [], [])",
    ]
    result = subprocess.run(insert, text=True)
    if result.returncode != 0:
        return result.returncode

    print(f"seeded batch_commit batch_id=0: pc={config.RAM_BASE:#x}, "
          f"{len(args.regs)} registers, icount=0", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
