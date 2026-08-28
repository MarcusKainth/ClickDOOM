# Write-log seeding: does per-instruction cost grow within a batch?

The instrument behind **#257**. Results live on that issue, not here — a
results file in-tree drifts from the issue that owns the decision
(`executor/bench/commit_mutation/README.md`'s rule). `executor/bench/hwm/RESULTS.md`
is not a counterexample: `executor/config.py` cites it *by path* as the
provenance for `WRITE_LOG_HIGH_WATER_MARK_DEFAULT`, so it has to live where the
pointer resolves. This harness lands no constant and no optimisation.

## The question

The batch-size sweep found an interior optimum near K ≈ 47,900. Fixed per-batch
costs alone make larger batches monotonically better, so an interior optimum
implies something in the batch grows superlinearly with batch length. Stepping
the fold is 91.9% of a batch, so if that term is inside the fold it is the
largest remaining lever on the project.

**#180 already answered this once** — it fitted a superlinear term and
attributed 10.3% of a K=60,000 boot batch to write-log growth. Its own caveat is
why this harness exists: *"Three parameters from three points is exactly
determined and would fit anything."* No repeats, no confidence interval, boot
window only.

## What it does that #180 could not

Varies write-log length **independently of K**. Nothing in normal operation can
do that — the log only grows by retiring stores — so `fold.select_only()` gained
a `wl0=` parameter that seeds the initial accumulator's write-log with `L0`
inert entries. Then K, the starting state, the query text and the compiled
lambda are all held constant and only `L0` moves. The slope
`d(fold_ms)/d(L0)` is read off directly instead of being fitted out of a
three-parameter model.

`slope / K` is the per-element, per-step cost. A linear scan predicts it is
**constant across K**, and that constancy — checked at several K — is the
mix-free answer to the headline question. It depends on no fit at all.

## Why not the obvious experiment

"Run single batches of length N from one checkpoint and fit `a + bN + cN²`"
compares *different instruction ranges* of a non-homogeneous window, so a fitted
curvature is partly instruction mix. `executor/bench/commit_mutation/ksweep.sh`'s
header makes this argument at length and is why that harness holds the window
fixed instead. Seeding sidesteps it: `L0` changes the write-log without changing
which instructions run.

## Why the seed is inert

`fold._addr_and_align`'s `wa_safe` clamps every word index with
`least(…, ram_words - 1)` — for *every* address, valid or not. So a real load's
`WA` is always `<= ram_words - 1`, and a seeded address of `UInt32::MAX` can
never satisfy `z = WA`. That is a property of the clamp, not of the address set
this ROM happens to touch, so it needs no empirical address analysis. The scan
is still walked in full: `arrayLastIndex` seeks the *last* match and cannot
short-circuit on a miss.

The structural argument is necessary but not sufficient, so it is also
executed — `executor/tests/test_fold.py::test_wl0_seed_is_inert_when_executed`
and `::test_wl0_seed_never_matches_a_real_load`. That has already earned its
keep; see below.

## The design that did not survive contact

The first version had five shapes, seeding acc.3's lanes *selectively*: lanes
`.1` and `.2` are both `Array(UInt32)` and only `.1` is scanned, so
`g_scan = g_V1 - g_V2` looked like a free way to split scan cost from copy cost.

**It is wrong.** `acc.3`'s lanes are parallel arrays and `LW` subscripts the
value lane with an index found in the address lane:

```sql
acc.3.2[arrayLastIndex(z -> z = WA, acc.3.1)]
```

Unequal lane lengths desynchronise them. With the addr lane seeded 8 deep and
the val lane empty, a real store lands at addr index 9 and val index 1, and a
load that should forward `0x1234` returns `0`. So an unequal seed is not inert
and would have timed a *different program* — silently, with a plausible-looking
number. The executed inertness test caught it on its first run.

Only equal-length seeds are valid, which is why `seed.py` has exactly two
shapes. `test_unequal_lane_seed_breaks_forwarding` pins the finding so the
elegant version cannot come back.

**Consequence:** the scan/copy split cannot come from subtraction inside the
fold. It comes from `micro.py` instead — standalone `arrayLastIndex` and
`arrayPushBack` throughput — compared against the measured slope. Two
instruments sharing no assumption is a real anchor; a subtraction between two
arms of one instrument was never going to be (#197).

## Inference, then observation

Comparing the sweep's slope against the primitive rates can only establish that
the fold is *cheaper* than a naive reading of its text implies. It cannot say
which of the two candidate reasons is responsible, because both produce the
same shortfall. So `micro.py` also carries two probes that test each hypothesis
**directly**, outside the fold:

- **H1 — does `arrayPushBack` copy the accumulator?**
  `arrayFold((acc, i) -> arrayPushBack(acc, i), range(N), [])` is O(N²) if the
  accumulator is copied each step and O(N) if it is mutated in place. The
  growth exponent over a sweep of N answers it, and the answer is 1 or 2 rather
  than a ratio needing interpretation.
- **H2 — does ClickHouse collapse the repeated scan?** Two folds over identical
  data, one evaluating `arrayLastIndex` once per step and one evaluating the
  byte-identical call six times. Equal cost means common subexpression
  elimination is firing — which is exactly the condition `LW`'s triple textual
  expansion creates in the real step expression, and what #191 separately found
  for the double scan.

The distinction matters for what may be claimed: without these, "CSE collapses
the scans and the accumulator is mutated in place" is an inference from three
measurements that happen to be consistent with it. With them it is observed.

## Files

| file | role |
|---|---|
| `seed.py` | the two valid seed shapes, and why the other three were removed |
| `bench_l0.py` | the sweep: per-arm `query_id`, `FORMAT Null`, K=0 probes, inertness assertions, live abort gates |
| `micro.py` | standalone element rates for `arrayLastIndex` and `arrayPushBack`, each gated on a linearity check |
| `fit_l0.py` | Theil–Sen slope, bootstrap CI, `slope/K` constancy, reconciliation against #180 and `micro.py` |

## Measurement discipline this encodes

**`select_only` writes nothing**, so consecutive arms are independent by
construction and a repeat is literally re-issuing the query. That is why one
container hosts the whole sweep, and it is a stronger guarantee than the
lambda-text identity argument #180 leaned on. It is *checked*, not trusted:
`ram`'s active part count must be exactly constant, and an interleaved V0
control must not drift more than 10%. Either failing aborts the block.

**`FORMAT Null` on every timed query.** The projection includes `wl_addr`,
`wl_val` and `wl_icount`, whose serialisation is O(L0). Without this the sweep
would partly measure result-set writing.

**The fixed cost is measured per arm, not assumed flat.** A `select_only(K=0)`
probe with the *identical* seed runs at every L0 and is subtracted. The seed
text does grow — by the decimal digits of L0 — and #180 established that ~92% of
the fixed cost is the analyzer walking generated SQL, so "the seed cannot affect
parse time" is a claim worth checking rather than asserting.

**The JIT regime is made uniform, not merely stated.** #180 could only report
its regime bias and argue it ran against the hypothesis. Here the sweep warms to
the compiled regime before the first recorded arm, and `CompileFunction` is
recorded per arm so the regime is visible next to every number (#166).

**HWM is raised to 200,000 and held constant.** The seeded `L0` counts toward
`length(acc.3.1) + 1 >= hwm`, so at the production 20,000 every seeded arm would
trip the mark on step 1. This is a deliberate deviation from
`config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT`; `hwm` *is* baked into the lambda, so
holding it constant across every arm is what keeps the compiled expression
identical.

**An aborted block reports as aborted.** Partial JSON plus the reason, never a
silent retry into a clean-looking result.

## Running it

Needs a pinned `rom/build/` and the pinned ClickHouse. Set up an isolated
database first (reusing `commit_mutation`'s sequencer — never a second copy):

    executor/bench/commit_mutation/setup_db.sh \
        --container clickdoom-ch --db wl257_boot --window boot

    # the sweep, one K
    python3 executor/bench/wl_seed/bench_l0.py --db wl257_boot --k 60000 \
        --reps 5 --out /tmp/wl257-boot-k60000.json

    # the attribution primitives
    python3 executor/bench/wl_seed/micro.py --out /tmp/wl257-micro.json

    # read the result
    python3 executor/bench/wl_seed/fit_l0.py /tmp/wl257-boot-k*.json \
        --micro /tmp/wl257-micro.json

or via `just bench-wl-seed`.

For the `slope/K` constancy check, run `bench_l0.py` at several K against the
same database and pass all the JSONs to `fit_l0.py` at once.
