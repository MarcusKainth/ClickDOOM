# Commit-path attribution, mutation cost, and the fold's fixed setup

The instrument behind **#180** (is the fold's per-batch setup a fixed cost
that a larger K would amortise? — **no**, see that issue's verdict) and the
baseline attribution on **#182**.

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
| `bench.py` | the measurement: N chained e2e batches, per-statement `query_id`, `part_log`/`system.mutations` dump, standalone `RAMT`/`DEC`/`KEYQ` timings, a direct `select_only(K=0)` reading of the fixed setup cost, write-log occupancy per batch; emits JSON |
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
wraps the `groupArray` in `length(...)` and asserts the result equals `ram`'s
row count, so a materialised 6.29M-element array is proven rather than
assumed. After the batches it must be checked against `count() FROM ram
FINAL`, not `count()` — every batch's flush appends a part, so the raw count
grows with the number of stores while the deduplicated count stays at SPEC
§2's 6,291,456; checking the raw count fails on a *correct* run. Retention
arms read `batch_commit`'s live row count *and* its
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

    # read the results
    python3 executor/bench/commit_mutation/fit.py /tmp/sq2-bench/*.json

or via `just bench-commit-attribution` / `just bench-ksweep`.

`bench.py` also carries `--wide-parts`, `--lightweight-deletes-sync`,
`--retention-every` and `--skip-retention`, which vary #182's retention
candidates end to end. **They are the wrong instrument for that question**
and are kept only for completeness: the retention DELETE measures 10-25 ms
against a ~26,700 ms batch, and batch-to-batch fold variance is ~700 ms, so
an end-to-end arm cannot resolve the effect at all — reporting a difference
from one would be reading noise. #187 settled those candidates properly, by
reading the `MutatePart` event's `read_bytes` out of `system.part_log`
instead of timing anything. `--skip-retention` is the useful one of the
four: it is the **upper bound** on what any retention change could recover.

Add `--window gameplay --snapshot <file>` to `arm.sh` to measure the
store-heavy gameplay window instead of boot; the snapshot is the one
`rom/bench/canonical_throughput` already caches per ROM.

## Results

Recorded on **#180** and **#182**, not here — a results file in-tree drifts
from the issue that owns the decision. Provenance for every number
(git SHA, ROM sha256, ClickHouse version, K/HWM, headroom, JIT regime) is in
each arm's JSON.
