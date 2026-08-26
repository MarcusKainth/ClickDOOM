# Commit-path attribution, mutation cost, and the fold's fixed setup

The instrument behind **#182** (does the `batch_commit` retention mutation
cost enough to be worth fixing?) and **#180** (is the fold's per-batch setup
a fixed cost that a larger K would amortise?).

Both questions need the same thing and neither could be answered by
`rom/bench/canonical_throughput`, which reports one wall-clock number per
window — the right instrument for "did throughput move", the wrong one for
"where inside the batch did the time go". So every statement here is issued
with its own `query_id` and reconciled against `system.query_log`
afterwards.

## What it does not reimplement

- the fold SQL — `executor/fold.py`'s `batch()`, unmodified;
- the four flushes — `executor/commit.py`'s generators, unmodified (#101:
  never hand-roll flush SQL);
- ROM load / decode / bootstrap — `setup_db.sh` is a thin sequencer over
  `sqlcpu/load_rom.py`, `sqlcpu/decode.sql` and `executor/bootstrap.py`;
- the gameplay-window snapshot — reuses
  `rom/bench/canonical_throughput/{gen,seed}_snapshot.py` and its cache.

## Files

| file | role |
|---|---|
| `arm.sh` | runs **one arm** in a **fresh container**; prints the idle-core headroom it observed and refuses below 8 idle cores |
| `setup_db.sh` | schema + ROM + decode + bootstrap (+ optional gameplay snapshot seed) into one isolated database |
| `bench.py` | the measurement: N chained e2e batches, per-statement `query_id`, `part_log`/`system.mutations` dump, standalone `RAMT` timing; emits JSON |
| `ksweep.sh` | #180's fixed-instruction-window K-sweep |
| `fit.py` | summarises arms and fits `S` from adjacent sweep arms |

## Measurement discipline this encodes

**Fresh container per arm, not a fresh database.** ClickHouse's
compiled-expression cache is server-global and keyed by each island's
ActionsDAG (#166). That is not a theoretical concern for a K-sweep — it is
load-bearing, and verifiably so:

```
$ python3 -c "import sys; sys.path.insert(0,'executor'); import fold
a = fold.build_step(2000,  0, 98824, 98824, 6291456, hwm=20000)
b = fold.build_step(60000, 0, 98824, 98824, 6291456, hwm=20000)
print(len(a), len(b), a == b)"
55295 55295 True
```

`K` only reaches `range(K)`, **outside** the lambda, so the step expression
— the thing that gets compiled and DAG-keyed — is byte-identical at every
K. One server would share a single cache key across an entire K-sweep, and
the first arm would warm it for all the others.

**Headroom, not container separation.** A private container isolates
ClickHouse state, not CPU. `arm.sh` reads `load1` against `hw.ncpu` before
touching anything and prints the idle-core count into the log next to the
numbers it qualifies.

**Regime is reported, never smoothed.** `min_count_to_compile_expression`
defaults to 3, so compilation lands on the 4th execution of a DAG.
`bench.py` reads `CompileFunction` / `CompileExpressionsMicroseconds` back
for every fold, so a number is always accompanied by the regime it was
taken in.

**The work is verified, not just the return.** The standalone `RAMT` timing
wraps the `groupArray` in `length(...)` and asserts the result equals
`ram`'s row count, so a materialised 6.29M-element array is proven rather
than assumed. Retention arms read `batch_commit`'s live row count *and* its
count at `apply_mutations_on_fly = 0`, plus `system.mutations`, because at
`lightweight_deletes_sync = 0` a statement returning is not evidence the
delete happened.

**No ratio from a single noisy pair.** `ksweep.sh` holds the executed
instruction window fixed and varies only the batch count, so each adjacent
arm pair gives an *independent* estimate of the per-batch setup `S`; `fit.py`
prints them individually and their spread is the error bar.

## Running it

Needs a pinned `rom/build/` (`just build-rom`) and Docker. Each arm creates
and destroys its own container, so **nothing else should be running on the
box** — check that first, per #182's protocol.

    # one arm
    executor/bench/commit_mutation/arm.sh --label baseline -- --k 60000 --batches 4

    # #180's sweep
    executor/bench/commit_mutation/ksweep.sh --window 120000

    # #182's candidates
    executor/bench/commit_mutation/arm.sh --label wide  -- --k 60000 --batches 20 --wide-parts
    executor/bench/commit_mutation/arm.sh --label async -- --k 60000 --batches 20 --lightweight-deletes-sync 0
    executor/bench/commit_mutation/arm.sh --label every16 -- --k 60000 --batches 20 --retention-every 16
    executor/bench/commit_mutation/arm.sh --label none  -- --k 60000 --batches 20 --skip-retention

    # read the results
    python3 executor/bench/commit_mutation/fit.py /tmp/sq2-bench/*.json

`--skip-retention` is not a candidate — it is the **upper bound** on what
any of #182's four changes could possibly recover, and it is the number to
look at first: if removing the statement outright is worth nothing, making
it cheaper is worth less than nothing.

Add `--window gameplay --snapshot <file>` to `arm.sh` to measure the
store-heavy gameplay window instead of boot; the snapshot is the one
`rom/bench/canonical_throughput` already caches per ROM.

## Results

Recorded on **#182** and **#180**, not here — a results file in-tree drifts
from the issue that owns the decision. Provenance for every number
(git SHA, ROM sha256, ClickHouse version, K/HWM, headroom, JIT regime) is in
each arm's JSON.
