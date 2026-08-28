#!/usr/bin/env python3
"""Generates the SPEC §7 checkpoint SELECT for the *latest* cpu_state row --
used by scripts/run_milestone.sh at every RAM_HASH_INTERVAL boundary to diff
the SQL CPU's state against refemu's committed reference trace.

Reuses sqlcpu/checkpoint.py's hash expressions verbatim rather than
reproducing them: that module already matches refemu's reg_hash()/
ram_hash()/fb_hash()/format_checkpoint() bit-for-bit (its own docstring:
"xxh64 seed 0 -- matches ClickHouse's xxHash64(x) with no seed argument"),
so there is never a second, hand-rolled implementation of a checkpoint line
to drift from the real one.

fb_hash (#210) reads the live `framebuffer`/`palette` tables, FINAL,
address-ascending -- like ramhash reads `ram` -- but densified over their
full declared word range first (`render.dense_words_sql()`): unlike
`ram`, these two tables start with zero rows and only gain one per
address after the ROM's first store there, so a bare FINAL read returns a
short or empty array before that, not a zero-filled one. Each dense word
array is turned into a raw byte string via driver/render.py's
`region_bytes_sql()` before handing both to `checkpoint.fb_hash()`.

`dense_words_sql()` was originally written *in this module* for exactly
this fb_hash path. #220 found the identical gap on `render.py`'s own
`frame_readout_sql()` (a bare `groupArray`/`FINAL` read of these same two
tables, dense only by accident of when `frame_readout_sql()` happens to
be called relative to `DG_DrawFrame`) and moved the function to
`driver/render.py` rather than adding a second copy there -- this module
already imports `render` for `FRAMEBUFFER_WORDS`/`PALETTE_WORDS`/
`region_bytes_sql()` and never the reverse, so `render.py` is the side
with no circular-import risk, and it already owns the two word-count
constants this function is parameterized by. This module now calls
`render.dense_words_sql()` instead of keeping its own copy.

Prints SQL to stdout, exactly like fold.py's/commit.py's own CLIs -- pipe
into clickhouse-client via stdin, not `--query` (the same ARG_MAX reasoning
every other script here that touches generated SQL already follows).

`reg_checkpoint_sql()` (#227/#230) is the cheap counterpart to
`checkpoint_sql()` above -- the CHECKPOINT_INTERVAL (4,096) cadence SPEC §7
defines as icount/pc/reghash only, with no ramhash/fbhash. It calls
`checkpoint.reg_hash(pc="pc", regs="regs")` with the *exact same arguments*
`checkpoint_sql()` does below, not a second, hand-written reghash
expression -- `scripts/test_checkpoint_query.py` asserts the two generated
SELECTs contain that identical substring, so a fork here can't happen
silently. Deliberately does NOT compute ramhash/fbhash: doing so would
mean hashing the full 24 MiB RAM region (plus framebuffer/palette) 256x
more often than SPEC §7 requires, which would make `scripts/diff_run.sh`'s
whole-trace comparison self-defeating on throughput for a cadence that
cannot see memory divergence either way (issue #191).

Usage: checkpoint_query.py --db DB [--reg-only]
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sqlcpu"))
import checkpoint

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "driver"))
import render


def checkpoint_sql(db: str) -> str:
    """The latest cpu_state row's SPEC §7 checkpoint line (all 5 fields --
    icount/pc/reghash/ramhash/fbhash), computed in one SELECT. The inner
    subquery pulls icount/pc/regs from the single latest row (ORDER BY
    batch_id DESC LIMIT 1 -- SPEC §5's own guidance: this read doesn't need
    FINAL, a duplicate pair at the same batch_id is content-identical
    either way) and computes reghash/ramhash/fbhash as ordinary columns
    referencing those same names, so the outer SELECT never has to reach
    into a nested tuple -- avoids ClickHouse's named-tuple-field-access
    edge cases entirely, at the cost of nothing (this runs once per
    checkpoint, not per instruction -- ADR-0002's per-node cost model has no
    bearing here). fbhash reads `framebuffer`/`palette` independently of
    the cpu_state row it's selected alongside -- same pattern ramhash
    already uses for `ram` above it, not a new one."""
    ram_words = f"(SELECT groupArray(value) FROM (SELECT value FROM {db}.ram FINAL ORDER BY word_addr))"
    fb_words = render.dense_words_sql(db, "framebuffer", render.FRAMEBUFFER_WORDS)
    pal_words = render.dense_words_sql(db, "palette", render.PALETTE_WORDS)
    reghash_expr = checkpoint.reg_hash(pc="pc", regs="regs")
    ramhash_expr = checkpoint.word_array_hash(ram_words)
    fbhash_expr = checkpoint.fb_hash(
        framebuffer=render.region_bytes_sql(fb_words),
        palette=render.region_bytes_sql(pal_words),
    )
    line = checkpoint.format_checkpoint(
        icount="icount", pc="pc", reghash="reghash", ramhash="ramhash", fbhash="fbhash"
    )
    return f"""SELECT {line}
FROM (
    SELECT icount, pc, regs,
           {reghash_expr} AS reghash,
           {ramhash_expr} AS ramhash,
           {fbhash_expr} AS fbhash
    FROM (SELECT icount, pc, regs FROM {db}.cpu_state ORDER BY batch_id DESC LIMIT 1)
)"""


def reg_checkpoint_sql(db: str) -> str:
    """The latest cpu_state row's SPEC §7 register-only checkpoint line
    (icount/pc/reghash) -- the CHECKPOINT_INTERVAL (4,096) cadence used by
    scripts/diff_run.sh (#227/#230) at every landing, versus
    checkpoint_sql()'s full 5-field line at every RAM_HASH_INTERVAL (256x
    rarer) landing. Same subquery shape as checkpoint_sql() above, minus
    the ram/framebuffer/palette reads -- and the SAME `checkpoint.reg_hash(
    pc="pc", regs="regs")` call, not a second expression that happens to
    look equivalent. A textual fork between this and checkpoint_sql()'s
    reghash would make every register-only comparison in diff_run.sh read
    as a false CPU divergence -- scripts/test_checkpoint_query.py asserts
    both SELECTs contain the identical expression string, not just that
    they agree on one sample input."""
    reghash_expr = checkpoint.reg_hash(pc="pc", regs="regs")
    line = checkpoint.format_checkpoint(icount="icount", pc="pc", reghash="reghash")
    return f"""SELECT {line}
FROM (
    SELECT icount, pc,
           {reghash_expr} AS reghash
    FROM (SELECT icount, pc, regs FROM {db}.cpu_state ORDER BY batch_id DESC LIMIT 1)
)"""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default="clickdoom")
    ap.add_argument("--reg-only", action="store_true",
                     help="emit the cheap CHECKPOINT_INTERVAL line (icount/pc/reghash) "
                          "instead of the full RAM_HASH_INTERVAL line")
    args = ap.parse_args()
    print(reg_checkpoint_sql(args.db) if args.reg_only else checkpoint_sql(args.db))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
