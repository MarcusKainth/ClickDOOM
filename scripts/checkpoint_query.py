#!/usr/bin/env python3
"""Generates the SPEC §7 checkpoint SELECT for the *latest* cpu_state row --
used by scripts/run_milestone.sh at every RAM_HASH_INTERVAL boundary to diff
the SQL CPU's state against refemu's committed reference trace.

Reuses sqlcpu/checkpoint.py's hash expressions verbatim rather than
reproducing them: that module already matches refemu's reg_hash()/
ram_hash()/format_checkpoint() bit-for-bit (its own docstring: "xxh64 seed
0 -- matches ClickHouse's xxHash64(x) with no seed argument"), so there is
never a second, hand-rolled implementation of a checkpoint line to drift
from the real one.

fb_hash is deliberately not wired in here -- it needs FRAMEBUFFER/PALETTE
SQL storage that doesn't exist in sqlcpu/schema.sql yet (#130 computes the
write-log lanes but nothing flushes them; #138's SPEC clauses are ratified
but the schema PR hasn't landed). Left as an obvious gap rather than guessed
at: this milestone run's job is reaching the target icount and reporting
(#110), fb_hash verification is #29's.

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
import checkpoint  # noqa: E402


def checkpoint_sql(db: str) -> str:
    """The latest cpu_state row's SPEC §7 checkpoint line, computed in one
    SELECT. The inner subquery pulls icount/pc/regs from the single latest
    row (ORDER BY batch_id DESC LIMIT 1 -- SPEC §5's own guidance: this read
    doesn't need FINAL, a duplicate pair at the same batch_id is
    content-identical either way) and computes reghash/ramhash as ordinary
    columns referencing those same names, so the outer SELECT never has to
    reach into a nested tuple -- avoids ClickHouse's named-tuple-field-access
    edge cases entirely, at the cost of nothing (this runs once per
    checkpoint, not per instruction -- ADR-0002's per-node cost model has no
    bearing here)."""
    ram_words = f"(SELECT groupArray(value) FROM (SELECT value FROM {db}.ram FINAL ORDER BY word_addr))"
    reghash_expr = checkpoint.reg_hash(pc="pc", regs="regs")
    ramhash_expr = checkpoint.word_array_hash(ram_words)
    line = checkpoint.format_checkpoint(icount="icount", pc="pc", reghash="reghash", ramhash="ramhash")
    return f"""SELECT {line}
FROM (
    SELECT icount, pc, regs,
           {reghash_expr} AS reghash,
           {ramhash_expr} AS ramhash
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
