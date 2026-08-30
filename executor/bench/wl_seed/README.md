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

or via `make bench-wl-seed`.

For the `slope/K` constancy check, run `bench_l0.py` at several K against the
same database and pass all the JSONs to `fit_l0.py` at once.
