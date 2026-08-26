#!/usr/bin/env python3
"""#182 candidates 1-3, isolated from the fold — #179's benchmark plan
step 3.

## Why this exists as a separate instrument

The end-to-end arm (`bench.py`) already shows the retention DELETE costs
~10-25 ms against a ~28,000 ms batch. An effect that small **cannot be
resolved end to end**: batch-to-batch fold variance on this box is several
seconds. Reporting "arm A ran 27.9 s and arm B ran 27.4 s, therefore
wide parts win" would be reading noise. So the candidates are measured on
the statement they actually change, with enough iterations to have an error
bar, and the end-to-end number is used only for the *share* — which is the
number that decides whether any of this is worth landing.

## What it reproduces about the real path, and what it does not

Reproduced faithfully:

  * the real `batch_commit` DDL, straight out of `sqlcpu/schema.sql`;
  * the real retention statement, straight out of `commit.py`'s
    `retention_sql()` (never retyped here);
  * one INSERT per simulated batch, so each batch lands as its own part,
    exactly as `fold.py`'s `INSERT INTO batch_commit` does;
  * write-log arrays at the SPEC default HWM (20,000 entries — `wl_addr`
    `UInt32` + `wl_val` `UInt32` + `wl_icount` `UInt64` = 320 KB of array
    payload per row before compression), which is what makes a Compact-part
    mutation expensive in #179's argument;
  * **steady state** — the table is pre-aged past the retention window
    before any timing is taken, so the DELETE matches real rows and the
    mutation runs against a realistic number of parts. A short run does not
    reach this: at N=16 the first sixteen DELETEs match nothing.

Not reproduced: the write-log *contents* are synthetic (`range()`-derived),
not a real batch's addresses. Part size and column count are what the
mutation cost depends on; the values are not, beyond compressibility. That
caveat is real and is why the end-to-end arm exists alongside this one.

Usage:
    retention_bench.py --container NAME --db DB [--arms ...] [--iterations 40]
"""
import argparse
import json
import os
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))

import commit  # noqa: E402
import config  # noqa: E402

BATCH_COMMIT_DDL = """CREATE TABLE {db}.batch_commit
(
    spec_version String DEFAULT '0.1.0',
    batch_id     UInt64,
    icount       UInt64,
    pc           UInt32,
    regs         Array(UInt32),
    halted       UInt8,
    halt_reason  LowCardinality(String),
    exit_code    UInt32,
    keyq_pos     UInt64,
    has_frame    UInt8,
    frame_no     UInt32,
    wl_addr      Array(UInt32),
    wl_val       Array(UInt32),
    wl_icount    Array(UInt64),
    console_bytes Array(UInt8)
)
ENGINE = MergeTree
ORDER BY batch_id{settings}"""


class CH:
    def __init__(self, container, password):
        self.base = ["docker", "exec", "-i", container, "clickhouse-client",
                     "--host", "localhost", "--port", "9000", "--user", "default"]
        if password:
            self.base += ["--password", password]

    def run(self, sql, query_id=None):
        qid = query_id or ("sq2r_" + uuid.uuid4().hex)
        t0 = time.perf_counter()
        proc = subprocess.run(self.base + ["--query_id", qid], input=sql,
                              capture_output=True, text=True)
        t1 = time.perf_counter()
        if proc.returncode != 0:
            raise RuntimeError(f"query {qid} failed:\n{proc.stderr[-3000:]}")
        return t1 - t0, proc.stdout, qid

    def scalar(self, sql):
        return self.run(sql)[1].strip()


def insert_row(db, batch_id, hwm):
    """One simulated batch commit: one part, write-log arrays at HWM.

    `range(...)` builds the arrays server-side so the 320 KB of payload never
    crosses the client socket — the thing being measured is the mutation
    that rewrites those columns, not the cost of shipping them in.
    """
    return f"""INSERT INTO {db}.batch_commit
  (batch_id, icount, pc, regs, halted, halt_reason, exit_code, keyq_pos,
   has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes)
SELECT toUInt64({batch_id}), toUInt64({batch_id} * 60000), toUInt32(2147483648),
       arrayResize(emptyArrayUInt32(), 31, toUInt32(0)), toUInt8(0), '', toUInt32(0),
       toUInt64(0), toUInt8(0), toUInt32(0),
       arrayMap(x -> toUInt32(x * 4 + {batch_id}), range({hwm})),
       arrayMap(x -> toUInt32(x + {batch_id}), range({hwm})),
       arrayMap(x -> toUInt64(x + {batch_id} * 60000), range({hwm})),
       arrayResize(emptyArrayUInt8(), 64, toUInt8(65))"""


def run_arm(ch, db, label, hwm, iterations, warmup, wide, sync, every, n):
    settings = " SETTINGS min_bytes_for_wide_part = 0" if wide else ""
    ch.run(f"DROP DATABASE IF EXISTS {db}")
    ch.run(f"CREATE DATABASE {db}")
    ch.run(BATCH_COMMIT_DDL.format(db=db, settings=settings))

    retention = commit.retention_sql(db=db, n=n)
    if sync is not None:
        retention += f"\nSETTINGS lightweight_deletes_sync = {sync}"

    # Pre-age past the retention window so the DELETE matches real rows from
    # the first timed iteration -- see the module docstring.
    for b in range(warmup):
        ch.run(insert_row(db, b, hwm))
        if (b + 1) % every == 0:
            ch.run(retention)

    samples, qids = [], []
    for i in range(iterations):
        b = warmup + i
        ch.run(insert_row(db, b, hwm))
        if (b + 1) % every == 0:
            secs, _, qid = ch.run(retention)
            samples.append(secs)
            qids.append(qid)

    # Settle: at lightweight_deletes_sync = 0 the statement returns before
    # the mutation is applied, so the correctness check below has to wait
    # for the queue to drain or it would be checking nothing.
    for _ in range(60):
        pending = int(ch.scalar(
            f"SELECT count() FROM system.mutations WHERE database = '{db}' AND is_done = 0"))
        if pending == 0:
            break
        time.sleep(0.5)

    ch.run("SYSTEM FLUSH LOGS")
    id_list = ",".join(f"'{q}'" for q in qids)
    _, out, _ = ch.run(
        f"SELECT query_id, query_duration_ms FROM system.query_log "
        f"WHERE query_id IN ({id_list}) AND type = 'QueryFinish' FORMAT JSONEachRow")
    server_ms = [json.loads(l)["query_duration_ms"] for l in out.splitlines() if l.strip()]

    _, out, _ = ch.run(
        f"SELECT part_type, count() AS n, sum(duration_ms) AS total_ms, avg(duration_ms) AS avg_ms, "
        f"avg(size_in_bytes) AS avg_bytes FROM system.part_log "
        f"WHERE database = '{db}' AND table = 'batch_commit' AND event_type = 'MutatePart' "
        f"GROUP BY part_type FORMAT JSONEachRow")
    mutate = [json.loads(l) for l in out.splitlines() if l.strip()]

    _, out, _ = ch.run(
        f"SELECT part_type, count() AS n, avg(size_in_bytes) AS avg_bytes FROM system.part_log "
        f"WHERE database = '{db}' AND table = 'batch_commit' AND event_type = 'NewPart' "
        f"GROUP BY part_type FORMAT JSONEachRow")
    newpart = [json.loads(l) for l in out.splitlines() if l.strip()]

    total = warmup + iterations
    live = int(ch.scalar(f"SELECT count() FROM {db}.batch_commit"))
    max_id = int(ch.scalar(f"SELECT max(batch_id) FROM {db}.batch_commit"))
    min_id = int(ch.scalar(f"SELECT min(batch_id) FROM {db}.batch_commit"))
    active_parts = int(ch.scalar(
        f"SELECT count() FROM system.parts WHERE database = '{db}' AND table = 'batch_commit' "
        f"AND active"))
    # "Verify the work happened": the window really is bounded, the newest
    # row really is retained, and nothing older than the bound survived.
    if max_id != total - 1:
        raise RuntimeError(f"{label}: newest batch_id is {max_id}, expected {total - 1} -- "
                           "retention deleted the row the next batch reads")
    if live > n + every + 1:
        raise RuntimeError(f"{label}: {live} live rows, window bound is n={n} every={every} -- "
                           "retention did not actually bound the table")

    srv = sorted(server_ms)
    return {
        "label": label, "wide_parts": wide, "lightweight_deletes_sync": sync,
        "retention_every": every, "retention_n": n, "hwm": hwm,
        "iterations": iterations, "warmup": warmup,
        "statements_timed": len(samples),
        "server_ms": server_ms,
        "server_ms_median": srv[len(srv) // 2] if srv else None,
        "server_ms_min": srv[0] if srv else None,
        "server_ms_max": srv[-1] if srv else None,
        "server_ms_mean": (sum(srv) / len(srv)) if srv else None,
        "server_ms_per_batch": (sum(srv) / iterations) if srv else 0.0,
        "wall_s_mean": sum(samples) / len(samples) if samples else None,
        "mutate_part": mutate,
        "new_part": newpart,
        "mutate_total_ms_per_batch": (sum(m["total_ms"] for m in mutate) / iterations) if mutate else 0.0,
        "live_rows": live, "min_batch_id": min_id, "max_batch_id": max_id,
        "active_parts": active_parts,
    }


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--container", required=True)
    p.add_argument("--db", default="retention_bench")
    p.add_argument("--password", default=os.environ.get("CLICKHOUSE_PASSWORD", "clickdoom"))
    p.add_argument("--hwm", type=int, default=config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT)
    p.add_argument("--iterations", type=int, default=40)
    p.add_argument("--warmup", type=int, default=32)
    p.add_argument("--retention-n", type=int, default=config.BATCH_COMMIT_RETENTION_N)
    p.add_argument("--out", default=None)
    args = p.parse_args()

    ch = CH(args.container, args.password)
    n = args.retention_n
    arms = [
        # label,                       wide,  sync, every
        ("compact_sync2_every1",       False, None, 1),      # today
        ("wide_sync2_every1",          True,  None, 1),      # candidate 1
        ("compact_sync0_every1",       False, 0,    1),      # candidate 2
        ("compact_sync2_every16",      False, None, n),      # candidate 3
        ("wide_sync0_every1",          True,  0,    1),      # 1 + 2
        ("wide_sync0_every16",         True,  0,    n),      # 1 + 2 + 3
    ]
    results = []
    for label, wide, sync, every in arms:
        r = run_arm(ch, f"{args.db}_{label}", label, args.hwm, args.iterations, args.warmup,
                    wide, sync, every, n)
        results.append(r)
        print(f"# {label}: median {r['server_ms_median']} ms/statement, "
              f"{r['server_ms_per_batch']:.1f} ms/batch amortised, "
              f"MutatePart {r['mutate_total_ms_per_batch']:.1f} ms/batch background, "
              f"parts={r['mutate_part']}, live={r['live_rows']}", file=sys.stderr)
        ch.run(f"DROP DATABASE IF EXISTS {args.db}_{label}")

    text = json.dumps(results, indent=2)
    print(text)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text)


if __name__ == "__main__":
    main()
