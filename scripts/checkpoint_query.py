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
full declared word range first (`dense_words_sql()` below): unlike `ram`,
these two tables start with zero rows and only gain one per address after
the ROM's first store there, so a bare FINAL read returns a short or empty
array before that, not a zero-filled one. Each dense word array is turned
into a raw byte string via driver/render.py's `region_bytes_sql()` before
handing both to `checkpoint.fb_hash()`. `region_bytes_sql()` is cited from
there rather than reimplemented (its own docstring explains why the
technique lives beside frame_readout_sql() and not here: it needs the
intermediate bytes, not just a hash of them) -- this module borrows the
one expression it needs, not the other way around.

Prints SQL to stdout, exactly like fold.py's/commit.py's own CLIs -- pipe
into clickhouse-client via stdin, not `--query` (the same ARG_MAX reasoning
every other script here that touches generated SQL already follows).

Usage: checkpoint_query.py --db DB
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sqlcpu"))
import checkpoint

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "driver"))
import render


def dense_words_sql(db: str, table: str, n_words: int) -> str:
    """A dense `Array(UInt32)` over `[0, n_words)` for `framebuffer`/
    `palette` -- tables that, unlike `ram` (zero-filled densely by
    `load_rom.py` at load time), start with ZERO ROWS and only gain one
    per address once the ROM's first store there actually happens. A bare
    `groupArray(value) FROM (... FINAL ORDER BY word_addr)` -- `ram_words`'s
    own pattern above -- silently returns a SHORT or EMPTY array before
    that first write, not a zero-filled one: an unwritten framebuffer/
    palette word must read as 0, the same "never-written memory is zero"
    semantic refemu's `Memory` gives both regions, for `fb_hash` to be
    comparable against the reference trace at all before the first
    `FRAME_COMMIT`.

    Caught by #210's own positive checkpoint test, not asserted from
    reasoning alone: an early-boot RAM_HASH_INTERVAL checkpoint's real
    fbhash (`7bed8159bb569479`, the pinned all-zero pre-render value 14 of
    15 lines in demo-boot-to-first-frame's reference trace carry) came
    back wrong the first time this ran, from exactly this gap -- `hex()`ing
    an empty array hashes the empty string, not 64,768 zero bytes.
    `LEFT JOIN` against a `numbers(n_words)` address domain, `coalesce`ing
    a missing match to 0, closes it; the ORDER BY lives on the inner
    subquery, same convention as `ram_words`, so `groupArray` sees rows in
    address order without needing its own ORDER BY (aggregates don't take
    one)."""
    return (
        f"(SELECT groupArray(value) FROM ("
        f"SELECT coalesce(t.value, 0) AS value "
        f"FROM (SELECT number AS word_addr FROM numbers({n_words})) n "
        f"LEFT JOIN (SELECT word_addr, value FROM {db}.{table} FINAL) t "
        f"ON n.word_addr = t.word_addr "
        f"ORDER BY n.word_addr"
        f"))"
    )


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
    fb_words = dense_words_sql(db, "framebuffer", render.FRAMEBUFFER_WORDS)
    pal_words = dense_words_sql(db, "palette", render.PALETTE_WORDS)
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


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default="clickdoom")
    args = ap.parse_args()
    print(checkpoint_sql(args.db))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
