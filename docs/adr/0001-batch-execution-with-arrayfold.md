# ADR-0001: Batch CPU execution via arrayFold with write-log memory

**Status:** accepted (Phase 0 benchmark met the threshold — see Phase 0 results below).
Amended by [ADR-0002](0002-predecoded-instruction-table.md), which supplies the
pre-decoding this decision turned out to require, and by
[ADR-0004](0004-halt-semantics-throughput-cost.md), which retires the ">=10,000
instr/sec sustained end-to-end" threshold below as a merge gate: it was set
before SPEC §1's halt semantics were priced in, and #23 measured 2,004
instr/sec e2e implementing them. The architecture (arrayFold, write-log,
K=50,000) stands; only the acceptance number is retired.

## Context

Two known ways to execute instructions in ClickHouse:

1. **Per-instruction materialized-view cascade** (Click-V's design): each
   clock INSERT drives one instruction through a chain of MVs. Proven, but
   every instruction pays full insert-pipeline overhead; estimated
   throughput is far below what DOOM needs (millions of instructions per
   frame).
2. **Batched `arrayFold`**: one SELECT executes K instructions in a native
   sequential fold. Accumulator carries `(pc, regs, write_log)`; full RAM is
   a captured constant array (O(1) `arrayElement` reads); stores append to
   the small write-log (reads check the log first, reverse-order); the log
   is flushed to the `ram` ReplacingMergeTree on batch commit.

ClickHouse arrays are immutable, so mutating a 24 MiB RAM array in the
accumulator would copy it every store — the write-log exists to keep the
mutable part of the accumulator small. K is capped so the O(log-length)
scans stay cheap.

## Decision

Adopt (2) as the executor architecture, with K default 50,000 and the batch
contract in SPEC §6.

## Consequences

- Throughput target: ≥10k instructions/sec sustained end-to-end (batch +
  commit + state reload). Below that, revisit.
- Risks to measure in Phase 0: accumulator copy behavior with a large
  captured constant array; write-log append cost growth within a batch;
  state reload cost per batch.
- Fallbacks, in order, if the benchmark fails: smaller K with amortized
  reload; recursive-CTE loop; paged RAM inside the accumulator (copy one
  page per store); per-instruction MV cascade as last resort (correct but
  slow — the project still works, the timelapse just gets longer).
- Whichever variant wins, the SPEC §6/§7 contracts hold, so `rom`, `refemu`
  and `sqlcpu` are insulated from the decision.

## Phase 0 results

Benchmarked on the repo pin (ClickHouse 26.3.17.4). Full tables, method and
caveats in [`docs/experiments/arrayfold-baseline.md`](../experiments/arrayfold-baseline.md);
reproduce with `make bench-phase0`.

**Verdict: accepted.** Sustained end-to-end throughput is **11,894
instructions/sec** at K=50,000, against this ADR's ">=10,000 instr/sec sustained
end-to-end (batch + commit + state reload)" threshold.

| K | instr/sec, end to end |
|---:|---:|
| 10,000 | 8,721 |
| 50,000 | **11,894** |
| 200,000 | 11,628 |

K=50,000 — the value this ADR guessed — is optimal, and for the reason it
guessed: below it the ~0.30s per-batch fixed cost dominates, above it the
write-log's growth cancels the amortization. **SPEC §6's K default stands.**

### On the three risks this ADR listed

- **Accumulator copy with a large captured constant array — not real.** Fold
  throughput is flat across a 6,144x range in the size of the captured RAM array
  (4 KiB to 24 MiB, 113,895 vs 106,951 instr/sec). Captured constants are not
  copied per step. Holding all of RAM as a query-level constant is sound.
- **Write-log append cost within a batch — real but bounded.** Growth is
  superlinear once the log gets long (at 25% stores: 2x K cost 2.4x then 2.8x
  the time). SPEC §6's high-water-mark early exit is what keeps it in the linear
  region, and it is load-bearing, not decorative.
- **State reload per batch — real, and the reason K matters.** ~0.15s to
  materialize the 24 MiB RAM constant plus ~0.017s for the decode arrays, per
  batch. Materialize with `FINAL`: measured 0.022-0.030s against 0.245-0.256s for
  `argMax(...) GROUP BY word_addr`, and `FINAL` stayed flat with 1.2M accumulated
  deltas.

### The risk this ADR did not anticipate

It assumed the cost model was about data movement. It is not: an `arrayFold` step
costs ~0.8us per *expression node* in the lambda, nearly independent of the data
those nodes touch. The obvious decode-in-the-lambda implementation runs at
**1,579 instr/sec** — six times below this ADR's own target — and no amount of
tuning K, threads, or `short_circuit_function_evaluation` moves it.

Moving decode into a table built inside ClickHouse takes the same fold to
**13,400 instr/sec**, a 7.4x speedup. That is what makes this decision viable, so
it is recorded as its own decision in ADR-0002 rather than buried here. None of
the fallbacks listed above were needed.

### What this costs the project in wall-clock

Worth stating plainly, because it is the number that decides how Phase 3 is
scheduled rather than how it is built. At ~12,000 instr/sec, a DOOM frame of
roughly 1-2M instructions takes **90-170 seconds**, and `-timedemo demo3` is a
multi-week continuous run. That is consistent with the README ("frame rate is
explicitly not a success criterion — the timelapse is the demo"), and the
project works at this speed; it is simply slow. The M-extension collapse
described in ADR-0002 is the most promising remaining lever.
