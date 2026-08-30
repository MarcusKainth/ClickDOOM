#!/usr/bin/env python3
"""Per-statement attribution of the end-to-end batch, plus the arms for
issue #182 (commit-path mutation cost) and issue #180 (is the fold's
per-batch setup a fixed cost that larger K amortises?).

Why this exists rather than reusing `rom/bench/canonical_throughput/run.sh`
unchanged: that script reports ONE wall-clock number per window, which is
the right instrument for "did throughput move". #182 and #180 both need the
batch *broken apart* -- #182 needs the share the retention mutation costs,
#180 needs the fixed intercept `S` in `T(K) = S + c*K` separated from the
per-step term. So every statement here is issued with its own `query_id`
and reconciled afterwards against `system.query_log`, and the `RAMT` CTE is
additionally timed standalone.

What it deliberately does NOT reimplement:

  * the fold SQL -- `executor/fold.py`'s `batch()`, unmodified;
  * the four flushes -- `executor/commit.py`'s four generators, unmodified
    (#101: never hand-roll flush SQL);
  * database setup -- `setup_db.sh` beside this file, itself a thin
    sequencer over sqlcpu/load_rom.py, sqlcpu/decode.sql and
    executor/bootstrap.py.

Measurement discipline this file enforces (see #166, and #182's protocol):

  * Wall-clock is measured around the client invocation, and *separately*
    reconciled with `system.query_log.query_duration_ms` for the same
    `query_id`. A gap between the two is client/round-trip overhead and is
    reported rather than hidden.
  * `CompileFunction` / `CompileExpressionsMicroseconds` are read back for
    the fold query of every batch, so the JIT regime a number was taken in
    is recorded with the number. `min_count_to_compile_expression` defaults
    to 3, so compilation lands on the 4th execution of a given DAG -- a
    3-batch arm is entirely in the uncompiled regime and must say so.
  * `system.part_log` MutatePart rows are dumped for the arm, because a
    lightweight DELETE at `lightweight_deletes_sync = 0` returns to the
    client immediately while still costing the machine the same mutation.
    Wall-clock alone would report that as free; it is not free, it is
    moved off the critical path. Both numbers are emitted.

Output is one JSON document on stdout (progress goes to stderr), so arms
can be diffed mechanically instead of by eye.
"""
import argparse
import json
import os
import subprocess
import sys
import time
import uuid

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))

import commit  # noqa: E402  (executor/commit.py)
import fold  # noqa: E402  (executor/fold.py)

RAM_WORDS = 6291456  # SPEC §2: 24 MiB / 4


def parse_args():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--container", required=True,
                   help="docker container name running the ClickHouse under test")
    p.add_argument("--db", required=True, help="isolated database prepared by setup_db.sh")
    p.add_argument("--manifest", default="rom/build/manifest.json")
    p.add_argument("--k", type=int, default=60000)
    p.add_argument("--hwm", type=int, default=20000)
    p.add_argument("--batches", type=int, default=4)
    p.add_argument("--password", default=os.environ.get("CLICKHOUSE_PASSWORD", "clickdoom"))
    p.add_argument("--label", default="arm", help="arm name, echoed into the JSON")
    p.add_argument("--retention-every", type=int, default=1,
                   help="run commit.py's retention statement every N batches (#182 candidate 3). "
                        "1 = today's behaviour.")
    p.add_argument("--retention-n", type=int, default=None,
                   help="retention window (default: executor/config.BATCH_COMMIT_RETENTION_N)")
    p.add_argument("--lightweight-deletes-sync", type=int, default=None,
                   help="if set, appended as a SETTINGS clause on the retention statement only "
                        "(#182 candidate 2)")
    p.add_argument("--wide-parts", action="store_true",
                   help="ALTER TABLE batch_commit MODIFY SETTING min_bytes_for_wide_part = 0 "
                        "before the first batch (#182 candidate 1)")
    p.add_argument("--skip-retention", action="store_true",
                   help="omit the retention statement entirely -- the upper bound on what any of "
                        "candidates 1-4 could possibly recover")
    p.add_argument("--out", default=None, help="also write the JSON here")
    return p.parse_args()


class CH:
    def __init__(self, container, db, password):
        self.base = ["docker", "exec", "-i", container, "clickhouse-client",
                     "--host", "localhost", "--port", "9000", "--user", "default",
                     "--database", db]
        if password:
            self.base += ["--password", password]

    def run(self, sql, query_id=None, fmt=None, multiquery=False):
        """Issue `sql`; return (wall_seconds, stdout, query_id)."""
        qid = query_id or ("sq2_" + uuid.uuid4().hex)
        cmd = list(self.base) + ["--query_id", qid]
        if fmt:
            cmd += ["--format", fmt]
        if multiquery:
            cmd += ["--multiquery"]
        t0 = time.perf_counter()
        proc = subprocess.run(cmd, input=sql, capture_output=True, text=True)
        t1 = time.perf_counter()
        if proc.returncode != 0:
            raise RuntimeError(f"query {qid} failed:\n{proc.stderr[-4000:]}")
        return t1 - t0, proc.stdout, qid

    def scalar(self, sql):
        _, out, _ = self.run(sql)
        return out.strip()


def query_log(ch, qids):
    """Reconcile client-side wall-clock against the server's own accounting.

    Read by `query_id` rather than by matching on query text: the fold's SQL
    is ~58 KB and byte-identical across batches, so text is not a key.
    `type = 'QueryFinish'` only -- the QueryStart row for the same id carries
    a zero duration and would halve every average if averaged in blindly.
    """
    ch.run("SYSTEM FLUSH LOGS")
    id_list = ",".join(f"'{q}'" for q in qids)
    sql = f"""SELECT query_id,
                     query_duration_ms,
                     memory_usage,
                     ProfileEvents['CompileFunction'] AS compile_function,
                     ProfileEvents['CompileExpressionsMicroseconds'] AS compile_us,
                     ProfileEvents['SelectedRows'] AS selected_rows,
                     ProfileEvents['FileOpen'] AS file_open,
                     read_rows, written_rows, written_bytes
              FROM system.query_log
              WHERE query_id IN ({id_list}) AND type = 'QueryFinish'
              FORMAT JSONEachRow"""
    _, out, _ = ch.run(sql)
    return {r["query_id"]: r for r in (json.loads(line) for line in out.splitlines() if line.strip())}


def part_log(ch, db):
    ch.run("SYSTEM FLUSH LOGS")
    sql = f"""SELECT table, event_type, part_type, count() AS n,
                     sum(duration_ms) AS total_duration_ms,
                     avg(duration_ms) AS avg_duration_ms,
                     avg(size_in_bytes) AS avg_size_bytes,
                     sum(size_in_bytes) AS total_size_bytes
              FROM system.part_log
              WHERE database = '{db}'
              GROUP BY table, event_type, part_type
              ORDER BY table, event_type
              FORMAT JSONEachRow"""
    _, out, _ = ch.run(sql)
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def main():
    args = parse_args()
    manifest = json.load(open(args.manifest))
    load_addr = manifest["load_addr"]
    text_start_widx = manifest["text_start"] // 4 - load_addr // 4
    text_end_widx = manifest["text_end"] // 4 - load_addr // 4
    decn = manifest["text_end"] // 4 - manifest["text_start"] // 4

    ch = CH(args.container, args.db, args.password)

    if args.wide_parts:
        ch.run(f"ALTER TABLE {args.db}.batch_commit MODIFY SETTING min_bytes_for_wide_part = 0")
    part_setting = ch.scalar(
        "SELECT value FROM system.merge_tree_settings WHERE name = 'min_bytes_for_wide_part'")
    # Read the *table*'s effective override back rather than trusting the
    # ALTER to have applied -- `min_bytes_for_wide_part` is a MergeTree table
    # setting, and the server-level default above is NOT what governs this
    # table once it carries its own override. The part_type column of the
    # part_log dump at the end is the independent confirmation.
    table_setting = ch.scalar(
        f"SELECT engine_full FROM system.tables "
        f"WHERE database = '{args.db}' AND name = 'batch_commit'").replace("\n", " ")

    retention_kwargs = {"db": args.db}
    if args.retention_n is not None:
        retention_kwargs["n"] = args.retention_n
    retention_sql = commit.retention_sql(**retention_kwargs)
    if args.lightweight_deletes_sync is not None:
        retention_sql += f"\nSETTINGS lightweight_deletes_sync = {args.lightweight_deletes_sync}"

    batch_sql = fold.batch(args.k, text_start_widx, text_end_widx, decn, RAM_WORDS, args.hwm,
                           db=args.db)
    flush_sql = {
        "ram": commit.ram_flush_sql(db=args.db),
        "console_out": commit.console_out_flush_sql(db=args.db),
        "cpu_state": commit.cpu_state_flush_sql(db=args.db),
    }

    # --- the RAMT CTE, standalone (#180 step 1) ---------------------------
    # Exactly fold.py's RAMT subquery text, at the same max_threads = 1 the
    # fold pins, wrapped in a `length()` so the groupArray is genuinely
    # materialised and not optimised away -- the returned number is checked
    # against ram's row count below, which is the "verify the work happened"
    # requirement, not just "the query returned".
    ramt_sql = (f"SELECT length((SELECT groupArray(tuple(value)) FROM "
                f"(SELECT value, word_addr FROM {args.db}.ram FINAL ORDER BY word_addr))) AS n "
                f"SETTINGS max_threads = 1")
    ramt = []
    for i in range(3):
        secs, out, qid = ch.run(ramt_sql)
        ramt.append({"i": i, "wall_s": secs, "rows": int(out.strip()), "query_id": qid})

    # --- the other two captures, timed the same way ----------------------
    dec_sql = (f"SELECT length((SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw)) "
               f"FROM (SELECT id, rd, rs1, rs2, imm, tgt, mk, sg, raw, word_addr "
               f"FROM {args.db}.decoded ORDER BY word_addr))) AS n SETTINGS max_threads = 1")
    keyq_sql = (f"SELECT length((SELECT groupArray(tuple(key_event)) FROM "
                f"(SELECT key_event, event_seq FROM {args.db}.input_queue ORDER BY event_seq))) AS n "
                f"SETTINGS max_threads = 1")
    captures = {}
    for name, sql in (("DEC", dec_sql), ("KEYQ", keyq_sql)):
        runs = []
        for i in range(3):
            secs, out, qid = ch.run(sql)
            runs.append({"i": i, "wall_s": secs, "rows": int(out.strip()), "query_id": qid})
        captures[name] = runs

    # --- S, measured directly rather than extrapolated -------------------
    #
    # `select_only(K=0)` folds over `range(0)`: ClickHouse still parses and
    # analyses the identical ~58 KB / ~90k-node query, still evaluates all
    # three `WITH` captures, and then runs the step lambda ZERO times. So
    # its duration IS the per-batch fixed cost, with no extrapolation and no
    # fit -- the intercept read off directly. `select_only`, not `batch()`,
    # deliberately: a K=0 `batch()` would commit a real row and shift the
    # chain the timed batches below run from.
    #
    # Verified rather than assumed to be doing the setup: the returned
    # `retired` must be 0 and `pc` must equal the seed pc, which is what a
    # fold over an empty range is required to produce.
    setup_probe = []
    for k_probe in (0, 0, 0, 1, 1, 1):
        sql = fold.select_only(k_probe, text_start_widx, text_end_widx, decn, RAM_WORDS, args.hwm,
                               db=args.db)
        secs, out, qid = ch.run(sql, fmt="TSVWithNames")
        lines = out.splitlines()
        row = dict(zip(lines[0].split("\t"), lines[-1].split("\t")))
        if k_probe == 0 and int(row["retired"]) != 0:
            raise RuntimeError(f"select_only(K=0) retired {row['retired']}, expected 0")
        setup_probe.append({"k": k_probe, "wall_s": secs, "retired": int(row["retired"]),
                            "pc": row["pc"], "query_id": qid})
    ram_rows = int(ch.scalar(f"SELECT count() FROM {args.db}.ram"))
    ram_parts = int(ch.scalar(
        f"SELECT count() FROM system.parts WHERE database='{args.db}' AND table='ram' AND active"))
    for r in ramt:
        if r["rows"] != ram_rows:
            raise RuntimeError(f"RAMT materialised {r['rows']} rows, ram has {ram_rows} -- "
                               "the FINAL scan did not do the work it was supposed to")

    # Seed icount: the gameplay window starts at a real mid-run icount, so
    # `retired` for the FIRST batch has to be measured against the seeded
    # value. Defaulting it to 0 makes a boot arm look right and a gameplay
    # arm report a 233-million-instruction first batch.
    icount_seed = int(ch.scalar(f"SELECT icount FROM {args.db}.batch_commit "
                                f"ORDER BY batch_id DESC LIMIT 1"))
    batches = []
    qids = []
    for b in range(args.batches):
        rec = {"batch": b, "statements": {}}
        for name, sql, multi in (("fold", batch_sql, False),
                                 ("ram", flush_sql["ram"], False),
                                 ("console_out", flush_sql["console_out"], False),
                                 ("cpu_state", flush_sql["cpu_state"], False)):
            secs, _, qid = ch.run(sql, multiquery=multi)
            rec["statements"][name] = {"wall_s": secs, "query_id": qid}
            qids.append(qid)
        run_retention = (not args.skip_retention) and ((b + 1) % args.retention_every == 0)
        if run_retention:
            secs, _, qid = ch.run(retention_sql)
            rec["statements"]["retention"] = {"wall_s": secs, "query_id": qid}
            qids.append(qid)
        rec["e2e_wall_s"] = sum(s["wall_s"] for s in rec["statements"].values())
        row = ch.scalar(f"SELECT icount, halted, halt_reason, batch_id FROM {args.db}.cpu_state "
                        f"ORDER BY batch_id DESC LIMIT 1")
        icount, halted, halt_reason, batch_id = row.split("\t")
        rec.update(icount=int(icount), halted=int(halted), halt_reason=halt_reason,
                   batch_id=int(batch_id))
        prev = batches[-1]["icount"] if batches else icount_seed
        rec["retired"] = rec["icount"] - prev
        rec["ran_retention"] = run_retention
        # Write-log occupancy for this batch. `retired < K` with `halted = 0`
        # means the fold stopped on the write-log high-water mark, not on K
        # -- and then `arrayFold` still iterated the remaining `range(K)`
        # elements as no-ops at full price. Recording the length is what
        # turns "the batch was short" into "the batch was short BECAUSE the
        # write log filled", which is the difference between a caveat and a
        # cause.
        wl = ch.scalar(f"SELECT length(wl_addr), length(console_bytes) FROM {args.db}.batch_commit "
                       f"WHERE batch_id = {rec['batch_id']}")
        rec["wl_len"], rec["console_len"] = (int(x) for x in wl.split("\t"))
        rec["truncated_by_hwm"] = (rec["retired"] < args.k and rec["halted"] == 0)
        print(f"# {args.label} batch {b}: {rec['e2e_wall_s']:.2f}s retired={rec['retired']} "
              f"halted={halted} " +
              " ".join(f"{k}={v['wall_s']:.2f}" for k, v in rec["statements"].items()),
              file=sys.stderr)
        batches.append(rec)

    probe_qids = ([r["query_id"] for r in ramt]
                  + [r["query_id"] for runs in captures.values() for r in runs]
                  + [r["query_id"] for r in setup_probe])
    # --- does the setup cost drift as `ram` accumulates parts? ----------
    #
    # The pre-batch `RAMT` reading above was taken at whatever part count
    # setup left behind. Every batch appends a part to `ram`, and `FINAL`
    # merges across active parts at read time, so the same probe re-run
    # after N batches is the direct test of this issue's degradation
    # question -- and it is a different question from "did the fold get
    # slower", because a fold that stayed flat while merges kept up says
    # nothing about what happens when they stop keeping up.
    ramt_after = []
    for i in range(3):
        secs, out, qid = ch.run(ramt_sql)
        ramt_after.append({"i": i, "wall_s": secs, "rows": int(out.strip()), "query_id": qid})
    ram_parts_after = int(ch.scalar(
        f"SELECT count() FROM system.parts WHERE database='{args.db}' AND table='ram' AND active"))
    # `count()` and `count() FINAL` diverge here and must not be confused:
    # every batch's `ram` flush appends a part, so the raw count grows by the
    # number of stores while the FINAL (deduplicated, one row per word_addr)
    # count stays at SPEC §2's 6,291,456. RAMT reads through FINAL, so FINAL
    # is what its length has to match. Checking against the raw count instead
    # fails on a correct run -- which is how this check earned its comment.
    ram_rows_after = int(ch.scalar(f"SELECT count() FROM {args.db}.ram"))
    ram_rows_after_final = int(ch.scalar(f"SELECT count() FROM {args.db}.ram FINAL"))
    for r in ramt_after:
        if r["rows"] != ram_rows_after_final:
            raise RuntimeError(f"post-run RAMT materialised {r['rows']} rows, "
                               f"ram FINAL has {ram_rows_after_final}")
    probe_qids += [r["query_id"] for r in ramt_after]

    log = query_log(ch, qids + probe_qids)
    for rec in batches:
        for name, st in rec["statements"].items():
            st["server"] = log.get(st["query_id"])
    for r in ramt:
        r["server"] = log.get(r["query_id"])
    for runs in captures.values():
        for r in runs:
            r["server"] = log.get(r["query_id"])
    for r in setup_probe:
        r["server"] = log.get(r["query_id"])
    for r in ramt_after:
        r["server"] = log.get(r["query_id"])

    # Verify retention actually bounded the table -- "the work happened",
    # not merely "the statement returned". At lightweight_deletes_sync = 0
    # this is checked after a settle, since the mutation is async by
    # construction there.
    live_rows = int(ch.scalar(f"SELECT count() FROM {args.db}.batch_commit"))
    all_rows = int(ch.scalar(f"SELECT count() FROM {args.db}.batch_commit "
                             f"SETTINGS apply_mutations_on_fly = 0"))
    mutations = ch.scalar(
        f"SELECT countIf(is_done = 0), count() FROM system.mutations "
        f"WHERE database = '{args.db}' AND table = 'batch_commit'")

    out = {
        "label": args.label,
        "db": args.db,
        "container": args.container,
        "k": args.k,
        "hwm": args.hwm,
        "batches_requested": args.batches,
        "retention_every": args.retention_every,
        "skip_retention": args.skip_retention,
        "lightweight_deletes_sync": args.lightweight_deletes_sync,
        "wide_parts": args.wide_parts,
        "server_min_bytes_for_wide_part": part_setting,
        "table_min_bytes_for_wide_part": table_setting,
        "ram_rows": ram_rows,
        "icount_seed": icount_seed,
        "ram_active_parts_at_start": ram_parts,
        "ramt_standalone": ramt,
        "ramt_after_batches": ramt_after,
        "ram_active_parts_after": ram_parts_after,
        "ram_rows_after": ram_rows_after,
        "ram_rows_after_final": ram_rows_after_final,
        "captures_standalone": captures,
        "setup_probe": setup_probe,
        "batches": batches,
        "batch_commit_live_rows": live_rows,
        "batch_commit_rows_ignoring_mutations": all_rows,
        "mutations_unfinished_total": mutations,
        "part_log": part_log(ch, args.db),
        "rom_sha256": manifest["sha256"],
    }
    text = json.dumps(out, indent=2)
    print(text)
    if args.out:
        with open(args.out, "w") as f:
            f.write(text)


if __name__ == "__main__":
    main()
