#!/usr/bin/env python3
"""Idempotent batch-commit flush and retention (#25, ADR-0003; #130/#160):
the four derivations that turn `batch_commit` -- the batch's single atomic
write, produced by `fold.py`'s `batch()` -- into
`ram`/`framebuffer`/`palette`/`console_out`/`cpu_state`'s observable state,
plus the retention statement that keeps `batch_commit` itself bounded.

All five statements here read `WHERE batch_id = (SELECT max(batch_id) FROM
batch_commit)` and nothing else -- no batch_id parameter, by design. That
means "run this after every batch" and "run this unconditionally on driver
startup, before any new batch, to recover from a crash" are the exact same
statement (ADR-0003: "no state machine, no partial-apply bookkeeping").
Each is safe to run any number of times for the same latest batch:

  * ram: `word_addr` is the flush's natural dedup key (ReplacingMergeTree),
    and `version` is the store's own absolute icount (fold.py's #101 fix),
    so a redone flush after a crash reinserts byte-identical rows.
  * fbpal (framebuffer/palette, #130/#160): same argument as ram --
    word_addr is each region's own natural dedup key, version is the
    store's own icount. Two INSERT statements (one per table), not one
    combined statement -- see fbpal_flush_sql()'s own docstring for why
    there's no rebasing step here unlike ram's.
  * console_out: `seq` is bitShiftLeft(batch_id, 32) + array position -- a
    pure function of (batch_id, position), so a redone flush is also
    byte-identical at the same key (ReplacingMergeTree, agreed with sqlcpu).
  * cpu_state: the whole row is a pure projection of one batch_commit row
    (ReplacingMergeTree keyed by batch_id, SPEC §5) -- same argument.

Usage: commit.py {ram,fbpal,console_out,cpu_state,retention} [--db DB] [--retention-n N]
"""
import argparse

import config

DB = "clickdoom"

LATEST_BATCH_ID = "(SELECT max(batch_id) FROM {db}.batch_commit)"


def ram_flush_sql(db=DB):
    """Flush this batch's write-log into `ram`. `wl_addr` is RAM_BASE-
    relative (fold.py's `wa_safe = (ADDR - RAM_BASE) >> 2`); `ram.word_addr`
    is absolute (sqlcpu/schema.sql: "byte address >> 2"). Adding
    `RAM_BASE >> 2` back on is load-bearing -- ADR-0003 spells this out
    because an earlier draft omitted it, and #81 is what a positionally-
    indexed `ram` array does when a flush gets this wrong: silent,
    deterministic corruption, no error anywhere.

    `t.1/t.2/t.3` reference one subquery-materialized `arrayJoin`, not three
    separate calls to it (contrast `executor/bench/batch_overhead`'s inline
    `arrayJoin(arrayZip(...)).1/.2/.3`, which re-evaluates the join per
    reference) -- cheaper and, since this isn't inside `arrayFold`,
    correctness-equivalent either way; the subquery form is just tidier."""
    ram_base_word = config.RAM_BASE >> 2
    latest = LATEST_BATCH_ID.format(db=db)
    return f"""INSERT INTO {db}.ram (word_addr, value, version)
SELECT {ram_base_word} + t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)) AS t
    FROM {db}.batch_commit
    WHERE batch_id = {latest}
)"""


def fbpal_flush_sql(db=DB):
    """Flush this batch's FRAMEBUFFER/PALETTE write-logs into `framebuffer`/
    `palette` (SPEC §5, #130/#160) -- the fourth idempotent derivation, same
    terms as ram_flush_sql above: ReplacingMergeTree keyed by word_addr, a
    redone flush after a crash reinserts byte-identical rows.

    UNLIKE ram_flush_sql, no RAM_BASE-style rebasing: fb_wl_addr/pal_wl_addr
    are already relative to each region's own base (fold.py's fb_wa/pal_wa,
    `bitShiftRight(ADDR - {FRAMEBUFFER,PALETTE}_BASE, 2)`), and
    `framebuffer`/`palette`.word_addr use that SAME region-relative
    convention (sqlcpu/schema.sql) -- there is no absolute domain on either
    side of this flush to reconcile, unlike ram's (wl_addr is RAM_BASE-
    relative, ram.word_addr is absolute, and the flush must add
    RAM_BASE >> 2 back on). One statement per region rather than one
    combined statement across both, since they are two separate tables
    (#130's write-frequency-asymmetry rationale) with two separate source
    array-triples -- there is no single arrayZip that spans both."""
    latest = LATEST_BATCH_ID.format(db=db)
    return f"""INSERT INTO {db}.framebuffer (word_addr, value, version)
SELECT t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(fb_wl_addr, fb_wl_val, fb_wl_icount)) AS t
    FROM {db}.batch_commit
    WHERE batch_id = {latest}
);
INSERT INTO {db}.palette (word_addr, value, version)
SELECT t.1, t.2, t.3
FROM (
    SELECT arrayJoin(arrayZip(pal_wl_addr, pal_wl_val, pal_wl_icount)) AS t
    FROM {db}.batch_commit
    WHERE batch_id = {latest}
)"""


def console_out_flush_sql(db=DB):
    """Flush this batch's console_bytes into `console_out`. `seq` is
    `bitShiftLeft(batch_id, 32) + (array position - 1)` -- agreed with
    sqlcpu over an earlier STRIDE-multiplier proposal, which they caught
    would silently collide (and drop bytes) if K ever grew past the
    constant. Bit-packing batch_id into the high 32 bits is collision-proof
    by construction: `demo3` is ~48,500 batches (nowhere near 2**32), and no
    batch's console output can approach 2**32 bytes (bounded by K, at most
    one PUTCHAR per retired instruction). `console_out` is
    `ReplacingMergeTree ORDER BY seq` (sqlcpu, follow-up to #92) so a redone
    flush is a harmless byte-identical duplicate at the same key, exactly
    like `ram`'s versioning."""
    latest = LATEST_BATCH_ID.format(db=db)
    return f"""INSERT INTO {db}.console_out (seq, byte)
SELECT bitShiftLeft(bc.batch_id, 32) + (t.1 - 1), t.2
FROM (
    SELECT batch_id, arrayJoin(arrayZip(arrayEnumerate(console_bytes), console_bytes)) AS t
    FROM {db}.batch_commit
    WHERE batch_id = {latest}
) AS bc"""


def cpu_state_flush_sql(db=DB):
    """Flush this batch's `cpu_state` row -- a pure projection of
    `batch_commit`'s matching seven columns, no unnesting, the cheapest of
    the three flushes. `cpu_state` is `ReplacingMergeTree(batch_id)`
    (sqlcpu, #92) so a redone flush after a crash leaves one row, not two;
    SPEC.md:153's guidance applies to *readers* of this table (use FINAL
    when row count/full history matters, not needed for the `ORDER BY
    batch_id DESC LIMIT 1` latest-state reload), not to this flush, which
    only ever produces one row per distinct batch_id regardless of how many
    times it runs."""
    latest = LATEST_BATCH_ID.format(db=db)
    return f"""INSERT INTO {db}.cpu_state (batch_id, icount, pc, regs, halted, halt_reason, exit_code)
SELECT batch_id, icount, pc, regs, halted, halt_reason, exit_code
FROM {db}.batch_commit
WHERE batch_id = {latest}"""


def retention_sql(db=DB, n=config.BATCH_COMMIT_RETENTION_N):
    """SPEC §5's batch_commit retention: drop whole rows older than
    `max(batch_id) - n`, batch-id lag not wall-clock time (ADR-0003).
    Lightweight `DELETE FROM` (not `ALTER TABLE ... DELETE`, which is a
    heavier async mutation) -- the exact form SPEC §5's own prose gives as
    an example.

    The signed-arithmetic detour (`toInt64`/`greatest`/`toUInt64`) guards a
    real UInt64 underflow: `batch_id` is UInt64, and on any of the first `n`
    batches of a run (batch_id=0..15 at the default N=16 -- true of every
    test run and every fresh start), `max(batch_id) - n` computed directly
    in UInt64 space wraps around to a huge value near UInt64::MAX instead of
    going negative. `batch_id < <huge wrapped value>` would then match every
    row in the table, including the batch just committed -- this statement
    would delete everything it was supposed to protect, on the very first
    batches of every run. Doing the subtraction in Int64, flooring at 0 with
    `greatest`, then casting back to UInt64 for the comparison avoids the
    wraparound entirely."""
    return f"""DELETE FROM {db}.batch_commit
WHERE batch_id < toUInt64(greatest(toInt64(0), toInt64((SELECT max(batch_id) FROM {db}.batch_commit)) - {n}))"""


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("which", choices=["ram", "fbpal", "console_out", "cpu_state", "retention"])
    p.add_argument("--db", default=DB,
                   help="database to generate SQL against (default: %(default)s). "
                        "Override for a benchmark run isolated onto its own database.")
    p.add_argument("--retention-n", type=int, default=config.BATCH_COMMIT_RETENTION_N,
                   help="retention only: batch-id lag to keep (default: %(default)s, SPEC §5)")
    args = p.parse_args()

    if args.which == "ram":
        print(ram_flush_sql(db=args.db))
    elif args.which == "fbpal":
        print(fbpal_flush_sql(db=args.db))
    elif args.which == "console_out":
        print(console_out_flush_sql(db=args.db))
    elif args.which == "cpu_state":
        print(cpu_state_flush_sql(db=args.db))
    else:
        print(retention_sql(db=args.db, n=args.retention_n))
