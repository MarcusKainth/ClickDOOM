"""Seed an isolated ClickHouse database's `ram`/`batch_commit` from a
`gen_snapshot.py` dump, so `run.sh` can benchmark the store-heavy gameplay
window without live-executing the ~234M instructions it takes to reach it.

Two inserts, deliberately kept as dumb as `sqlcpu/load_rom.py`'s own ROM
load -- reinterpreting already-computed bytes and inserting them at their
address, never inspecting what a word *means*:

1. `ram`: the snapshot's full 24 MiB RAM dump, one row per word --
   `load_rom.py`'s exact TSV-over-stdin technique (the only way to move a
   few million rows in one client round trip), just from the snapshot's
   dense array instead of `image + zero-fill`. The snapshot IS already
   dense over the whole RAM region (refemu's `Memory.ram` is a flat
   `bytearray(RAM_SIZE)` from construction), so there is no separate
   zero-fill step and no density gap to hit #81's bug -- it is checked
   anyway, the same way `load_rom.py` checks its own load, because a
   single dropped byte-range here would silently feed the wrong RAM state
   into every subsequent measurement.
2. `batch_commit`'s batch_id=0 row: same shape as `executor/bootstrap.py`'s
   SPEC §1 reset-state insert, with the snapshot's icount/pc/regs instead
   of the reset values. NOT a change to `bootstrap.py` itself --
   `bootstrap.py`'s docstring is explicit that it seeds *reset* state and
   is meant to run "once, ever, per fresh run"; this is a different
   contract (an arbitrary mid-run snapshot, benchmark-only), so it gets
   its own insert rather than overloading `bootstrap.py`'s narrower one.
   `wl_addr`/`wl_val`/`wl_icount`/`console_bytes` are seeded empty and
   `keyq_pos`/`has_frame`/`frame_no` at 0 -- gen_snapshot.py's own
   docstring explains why (no FRAMEBUFFER/PALETTE/console SQL storage
   exists yet, and this benchmark only needs pc/regs/ram to measure
   throughput, not exact MMIO continuity).

Usage:
    python3 seed_snapshot.py --snapshot /tmp/.../snapshot.eabb12ed4f18.233932753.pkl \\
        --database rom_bench_gameplay --client "docker exec -i clickdoom-ch clickhouse-client"
"""

from __future__ import annotations

import argparse
import pickle
import struct
import subprocess
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--snapshot", required=True)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", required=True)
    ap.add_argument("--client", default="clickhouse-client")
    args = ap.parse_args()

    with open(args.snapshot, "rb") as f:
        state = pickle.load(f)

    ram_bytes = state["ram"]
    if len(ram_bytes) % 4 != 0:
        print(f"::error::snapshot ram is {len(ram_bytes)} bytes, not a multiple of 4", file=sys.stderr)
        return 1
    ram_base = state["ram_base"]
    base_word = ram_base >> 2
    nwords = len(ram_bytes) // 4
    words = struct.unpack(f"<{nwords}I", ram_bytes)

    base_cmd = args.client.split() + [
        "--host", args.host,
        "--port", str(args.port),
        "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    # Same TSV-over-stdin technique as sqlcpu/load_rom.py, same reason:
    # this is a few million rows, well past what `--query`'s ARG_MAX can
    # carry as literal SQL.
    rows = "\n".join(f"{base_word + i}\t{w}\t0" for i, w in enumerate(words))
    insert_ram = base_cmd + ["--query", "INSERT INTO ram (word_addr, value, version) FORMAT TSV"]
    result = subprocess.run(insert_ram, input=rows, text=True)
    if result.returncode != 0:
        return result.returncode
    print(f"seeded ram: {nwords} words at word_addr {base_word}..{base_word + nwords - 1} "
          f"(icount={state['icount']:,})", file=sys.stderr)

    check = subprocess.run(
        base_cmd + ["--query", "SELECT count(), max(word_addr) - min(word_addr) + 1, min(word_addr) FROM ram FINAL"],
        text=True, capture_output=True,
    )
    if check.returncode != 0:
        print(check.stderr, file=sys.stderr, end="")
        return check.returncode
    rows_n, span, lowest = (int(x) for x in check.stdout.split())
    if rows_n != span or rows_n != nwords or lowest != base_word:
        print(f"::error::seeded ram is not dense: {rows_n} rows spanning {span} words from {lowest}, "
              f"expected {nwords} rows spanning {nwords} from {base_word}. RAMT indexes positionally "
              f"-- see #81.", file=sys.stderr)
        return 1
    print(f"ram dense: {rows_n} rows, word_addr {lowest}..{lowest + rows_n - 1} -- OK", file=sys.stderr)

    regs31 = state["regs"][1:32]  # drop refemu's x0 (always 0, not stored -- schema.sql's convention)
    if len(regs31) != 31:
        print(f"::error::snapshot regs has {len(state['regs'])} elements, expected 32 (x0..x31)", file=sys.stderr)
        return 1
    regs_literal = "[" + ",".join(str(r) for r in regs31) + "]"
    insert_batch_commit = base_cmd + [
        "--query",
        "INSERT INTO batch_commit "
        "(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
        " keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
        f"VALUES (0, {state['icount']}, {state['pc']}, {regs_literal}, 0, '', 0, "
        f" 0, 0, 0, [], [], [], [])",
    ]
    result = subprocess.run(insert_batch_commit, text=True)
    if result.returncode != 0:
        return result.returncode
    print(f"seeded batch_commit batch_id=0: pc={state['pc']:#x}, icount={state['icount']:,}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
