# ADR-0001: Batch CPU execution via arrayFold with write-log memory

**Status:** proposed — acceptance gated on the Phase 0 benchmark (SPEC §9).

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
