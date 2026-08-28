#!/usr/bin/env python3
"""#257's write-log seed sweep: does per-step cost grow with write-log length?

Runs `fold.select_only()` from ONE fixed machine state with the write-log
pre-seeded to L0 entries, and times it. K, the starting state, the query text
and the compiled lambda are all held constant; only L0 varies. The slope
d(fold_ms)/d(L0) is the per-element cost of carrying a longer write-log, with
instruction mix controlled exactly -- every arm executes the identical
`range(K)` iterations from the identical state.

## Why this is the right instrument, and the naive one is not

The obvious experiment -- "run single batches of length N from one checkpoint
and fit a + bN + cN^2" -- compares DIFFERENT instruction ranges of a
non-homogeneous window, so a fitted curvature is partly instruction mix.
`executor/bench/commit_mutation/ksweep.sh`'s header comment makes this argument
at length and it is why that harness holds the window fixed. Seeding sidesteps
it entirely: L0 changes the write-log without changing which instructions run.

## Why one container hosts the whole sweep

`select_only` WRITES NOTHING -- no commit, no flush, no part appended to `ram`.
So consecutive arms are independent by construction and a repeat is literally
re-issuing the same query. That is a stronger guarantee than the lambda-text
identity argument #180 relied on, and it is why this sweep does not need
#166's fresh-container-per-arm discipline (which exists for arms that mutate
state, and which `ksweep.sh` still needs).

The claim is checked rather than trusted: `ram`'s active part count must be
EXACTLY constant across the run, and an interleaved V0 control arm must not
drift. Either failing aborts the block.

## Timing source

Server-side `system.query_log.query_duration_ms`, keyed by `query_id` -- not a
client-side wall clock, which would fold in `docker exec` and round-trip cost.
Every timed query is issued `FORMAT Null`: the projection includes `wl_addr`,
`wl_val` and `wl_icount`, whose serialisation is O(L0), so without this the
sweep would partly measure result-set writing rather than folding.

The fixed per-batch cost (parse, analyse, the three `WITH` captures) is
measured per arm by a `select_only(K=0)` probe with the IDENTICAL seed, and
subtracted. It is measured rather than assumed flat because the seed text does
grow -- by the decimal digits of L0 -- and #180 established that ~92% of the
fixed cost is the analyzer walking generated SQL.

Determinism (SPEC §8): no host clock or randomness on any path that affects a
reported number. `os.getloadavg` is read only as a contention guard, and its
value gates whether a measurement is taken, never what the measurement says.
"""
import argparse
import json
import os
import subprocess
import sys
import uuid

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.dirname(__file__))
import fold  # noqa: E402
import seed  # noqa: E402


class CH:
    def __init__(self, container, db, password):
        self.base = ["docker", "exec", "-i", container, "clickhouse-client",
                     "--host", "localhost", "--port", "9000", "--user", "default",
                     "--database", db]
        if password:
            self.base += ["--password", password]

    def run(self, sql, query_id=None, fmt=None, settings=()):
        qid = query_id or ("wl257_" + uuid.uuid4().hex)
        cmd = list(self.base) + ["--query_id", qid]
        for s in settings:
            cmd += [f"--{s}"]
        if fmt:
            cmd += ["--format", fmt]
        proc = subprocess.run(cmd, input=sql, capture_output=True, text=True)
        if proc.returncode != 0:
            raise RuntimeError(f"query {qid} failed:\n{proc.stderr[-3000:]}")
        return proc.stdout.strip(), qid

    def scalar(self, sql):
        return self.run(sql)[0]

    def stats(self, qid):
        self.run("SYSTEM FLUSH LOGS")
        out, _ = self.run(
            "SELECT query_duration_ms, memory_usage, "
            "ProfileEvents['CompileFunction'] AS cf, "
            "ProfileEvents['CompileExpressionsMicroseconds'] AS cus "
            f"FROM system.query_log WHERE query_id = '{qid}' "
            "AND type = 'QueryFinish' FORMAT JSONEachRow")
        rows = [json.loads(x) for x in out.splitlines() if x.strip()]
        if not rows:
            raise RuntimeError(f"no QueryFinish row for {qid}")
        return rows[0]


class Aborted(Exception):
    """A live gate tripped. Partial results are still written."""


class Sweep:
    def __init__(self, ch, args):
        self.ch, self.a = ch, args
        self.records = []
        self.baseline_v0_ms = None
        self.ram_parts0 = self._ram_parts()
        self.step_sha = None

    # -- environment guards ------------------------------------------------
    def _ram_parts(self):
        return int(self.ch.scalar(
            "SELECT count() FROM system.parts WHERE active AND table = 'ram' "
            f"AND database = '{self.a.db}'"))

    def _idle_cores(self):
        return os.cpu_count() - os.getloadavg()[0]

    def _check_environment(self, where):
        idle = self._idle_cores()
        if idle < self.a.min_idle_cores:
            raise Aborted(f"{where}: only {idle:.1f} idle cores "
                          f"(need {self.a.min_idle_cores}) -- a timing taken here "
                          f"would be measuring someone else's work")
        parts = self._ram_parts()
        if parts != self.ram_parts0:
            raise Aborted(f"{where}: ram active parts moved {self.ram_parts0} -> "
                          f"{parts}. select_only writes nothing, so something else "
                          f"is touching {self.a.db} and every number after this "
                          f"point is void")

    # -- one arm -----------------------------------------------------------
    def _sql(self, shape, l0, k):
        return fold.select_only(k, 0, self.a.decn, self.a.decn, self.a.ram_words,
                                self.a.hwm, db=self.a.db,
                                wl0=seed.seed_sql(shape, l0))

    def capture(self, shape, l0, k):
        """Architectural state after the fold, as scalars and array hashes.

        Hashed rather than returned whole: at L0 = 80,000 the raw arrays are
        megabytes, and only their equality matters. `arraySlice(.., L0+1)`
        drops the seeded prefix so the REAL stores can be compared against the
        unseeded run directly.
        """
        pre = seed.seeded_len(shape, l0)
        inner = self._sql(shape, l0, k)
        sql = (f"SELECT pc, retired, halted, halt_reason, stopped, keyq_pos, "
               f"frame_no, frame_committed, cityHash64(regs) AS rh, "
               f"length(wl_addr) AS wl_len, "
               f"cityHash64(arraySlice(wl_addr, {pre + 1})) AS wah, "
               f"cityHash64(arraySlice(wl_val, {pre + 1})) AS wvh, "
               f"cityHash64(arraySlice(wl_icount, {pre + 1})) AS wih "
               f"FROM (\n{inner}\n)")
        out, _ = self.ch.run(sql, fmt="JSONEachRow")
        return json.loads(out.splitlines()[0])

    def timed(self, shape, l0, k, label):
        sql = self._sql(shape, l0, k)
        _, qid = self.ch.run(sql, fmt="Null")
        st = self.ch.stats(qid)
        rec = {"label": label, "shape": shape, "l0": l0, "k": k,
               "duration_ms": st["query_duration_ms"],
               "memory_usage": st["memory_usage"],
               "compile_function": st["cf"], "compile_us": st["cus"],
               "query_id": qid}
        self.records.append(rec)
        return rec

    # -- assertions --------------------------------------------------------
    def assert_lambda_stable(self, shape, l0):
        """A5: the seed must not reach the compiled lambda.

        A per-call-varying literal inside the lambda body is part of
        ClickHouse's compiled-expression cache key and silently disables the
        JIT -- the exact bug build_step()'s docstring records for icount0. The
        step expression must therefore be byte-identical at every L0.
        """
        step = fold.build_step(self.a.k, 0, self.a.decn, self.a.decn,
                               self.a.ram_words, hwm=self.a.hwm)
        import hashlib
        sha = hashlib.sha256(step.encode()).hexdigest()
        if self.step_sha is None:
            self.step_sha = sha
        elif sha != self.step_sha:
            raise Aborted(f"step expression changed at {shape}/L0={l0}")
        if step not in self._sql(shape, l0, self.a.k):
            raise Aborted(f"seed leaked into the lambda at {shape}/L0={l0}")

    def assert_inert(self, base, got, shape, l0):
        """A2/A6: the seed changed nothing, and no arm was HWM-truncated."""
        for f in ("pc", "retired", "halted", "halt_reason", "stopped",
                  "keyq_pos", "frame_no", "frame_committed", "rh",
                  "wah", "wvh", "wih"):
            if got[f] != base[f]:
                raise Aborted(
                    f"{shape}/L0={l0}: {f} differs from the unseeded run "
                    f"({got[f]} vs {base[f]}). The seed is NOT inert, so this "
                    f"arm timed a different program and the sweep is void")
        exp_len = base["wl_len"] + seed.seeded_len(shape, l0)
        if got["wl_len"] != exp_len:
            raise Aborted(f"{shape}/L0={l0}: wl_len {got['wl_len']} != {exp_len}")
        if got["retired"] != self.a.k or got["halted"] != 0:
            raise Aborted(
                f"{shape}/L0={l0}: retired={got['retired']} halted={got['halted']} "
                f"-- expected a full K={self.a.k} with no halt. An HWM-truncated "
                f"arm executes different work and cannot be compared")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--container", default="clickdoom-ch")
    p.add_argument("--db", required=True)
    p.add_argument("--password", default=os.environ.get("CLICKHOUSE_PASSWORD", "clickdoom"))
    p.add_argument("--k", type=int, default=60000)
    # Raised well above the production 20,000: the seeded L0 counts toward
    # `length(acc.3.1) + 1 >= hwm`, so at the default every arm with a seed
    # would trip the mark on step 1. Held CONSTANT across the sweep, which
    # matters because hwm IS baked into the lambda text.
    p.add_argument("--hwm", type=int, default=200000)
    p.add_argument("--l0", default="0,5000,10000,20000,40000,80000")
    p.add_argument("--reps", type=int, default=5)
    p.add_argument("--decn", type=int, default=98953)
    p.add_argument("--ram-words", type=int, default=6291456)
    p.add_argument("--warmups", type=int, default=4,
                   help="discarded folds to reach a uniform compiled regime "
                        "(min_count_to_compile_expression defaults to 3)")
    p.add_argument("--min-idle-cores", type=float, default=8.0)
    p.add_argument("--control-every", type=int, default=10,
                   help="re-run the V0 control every N arms; >10%% drift aborts")
    p.add_argument("--out", default=None)
    a = p.parse_args()

    l0s = [int(x) for x in a.l0.split(",")]
    ch = CH(a.container, a.db, a.password)
    sw = Sweep(ch, a)
    # Provenance. Every number this project has retracted lost its meaning by
    # being separated from what produced it (canonical_throughput/README.md).
    def _sh(cmd):
        try:
            return subprocess.run(cmd, capture_output=True, text=True,
                                  check=True).stdout.strip()
        except Exception:
            return None

    here = os.path.dirname(os.path.abspath(__file__))
    result = {"db": a.db, "k": a.k, "hwm": a.hwm, "l0": l0s, "reps": a.reps,
              "decn": a.decn, "ram_words": a.ram_words, "warmups": a.warmups,
              "clickhouse_version": ch.scalar("SELECT version()"),
              "git_sha": _sh(["git", "-C", here, "rev-parse", "HEAD"]),
              "rom_pinned_hash": _sh(["cat", os.path.join(here, "..", "..", "..",
                                                          "rom", "PINNED_HASH")]),
              "idle_cores_at_start": os.cpu_count() - os.getloadavg()[0],
              "ram_active_parts_at_start": sw.ram_parts0,
              "aborted": None, "records": sw.records}

    try:
        sw._check_environment("start")

        # Warm to a uniform compiled regime. #180's sweep mixed regimes and
        # could only state the resulting bias; this removes it instead.
        for i in range(a.warmups):
            sw.timed("V0", 0, a.k, f"warmup{i}")
        sw.records.clear()

        # A2 baseline: the unseeded run's own state, which every seeded arm
        # must reproduce exactly.
        base = sw.capture("V0", 0, a.k)
        result["baseline_state"] = base

        arms = 0
        for l0 in l0s:
            shape = "V0" if l0 == 0 else "VA"
            sw.assert_lambda_stable(shape, l0)
            got = sw.capture(shape, l0, a.k)
            sw.assert_inert(base, got, shape, l0)

            probe = sw.timed(shape, l0, 0, f"probe_L{l0}")
            for r in range(a.reps):
                sw._check_environment(f"L0={l0} rep{r}")
                rec = sw.timed(shape, l0, a.k, f"L{l0}_r{r}")
                rec["probe_ms"] = probe["duration_ms"]
                rec["net_ms"] = rec["duration_ms"] - probe["duration_ms"]
                arms += 1
                if arms % a.control_every == 0:
                    c = sw.timed("V0", 0, a.k, f"control_after{arms}")
                    if sw.baseline_v0_ms is None:
                        sw.baseline_v0_ms = c["duration_ms"]
                    elif abs(c["duration_ms"] - sw.baseline_v0_ms) > 0.10 * sw.baseline_v0_ms:
                        raise Aborted(
                            f"V0 control drifted {sw.baseline_v0_ms} -> "
                            f"{c['duration_ms']} ms (>10%). The machine changed "
                            f"under the sweep; later arms are not comparable")
            if sw.baseline_v0_ms is None and l0 == 0:
                sw.baseline_v0_ms = min(r["duration_ms"] for r in sw.records
                                        if r["l0"] == 0 and r["k"] == a.k)
    except Aborted as e:
        result["aborted"] = str(e)
        print(f"::error::ABORTED -- {e}", file=sys.stderr)

    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f, indent=2)

    # Per-L0 medians of the net (fold-only) time.
    print(f"\n{'L0':>8} {'n':>3} {'median_ms':>10} {'net_ms':>9} {'probe_ms':>9} {'CompFn':>7}")
    by = {}
    for r in sw.records:
        if r["k"] == a.k and r["label"].startswith("L"):
            by.setdefault(r["l0"], []).append(r)
    for l0 in sorted(by):
        rs = sorted(by[l0], key=lambda r: r["net_ms"])
        m = rs[len(rs) // 2]
        print(f"{l0:>8} {len(rs):>3} {m['duration_ms']:>10} {m['net_ms']:>9} "
              f"{m['probe_ms']:>9} {m['compile_function']:>7}")
    if result["aborted"]:
        sys.exit(1)


if __name__ == "__main__":
    main()
