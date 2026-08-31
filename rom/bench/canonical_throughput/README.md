# Canonical real-ROM throughput benchmark

## Why this exists

The human owner ruled on #130's 1.61x throughput regression (recorded in
#147): the baseline benchmark's "1,000 instr/sec floor" was an architecture-viability
tripwire, not a merge gate for correctness-driven cost with the
optimisation queue unexecuted. That triggered a **time-boxed 5-day
optimisation sprint**, after which the floor is re-evaluated for real.

Every measurement this project has taken so far has been ad hoc, and every
one has cost something: contaminated by a concurrent process, taken at a
non-optimal K, taken against a stale ROM, taken on a synthetic fixture that
mispredicted the real ROM's behaviour by the wrong sign. Each was caught,
but each cost a re-run, and some cost a wrong conclusion first. **A sprint
whose dozen experiments are each measured differently cannot be summed.**
This is the one instrument every sprint number should come from.

## What it measures

Two windows, identified from the memory-function profile's exact per-symbol
attribution against the current frozen ROM (`rom/PINNED_HASH`):

| window | starts at | what's happening |
|---|---|---|
| boot-phase | icount 0 | WAD load, `R_Init*`, `strncasecmp`-heavy lump lookups -- read/scan-dominated |
| store-heavy gameplay | icount 233,932,753 | real `-timedemo demo3` playback -- `R_DrawColumn`/`R_DrawSpan` dominate, both pixel-store-bound rasterizers |

One blended whole-run average would hide exactly the effect `executor`
found in #130: added correctness checks compound on memory-heavy code
rather than diluting evenly across the instruction stream. The two windows
are reported separately so that effect is visible instead of averaged away.

Each window is measured two ways, reported separately (ADR-0004's own
convention):

- **fold-alone** -- `executor::fold::select_only`, the cost of the
  `arrayFold` step expression itself. Its write logs are applied to
  `ram`/`framebuffer`/`palette` between chained batches, outside the timed
  statement, so a later batch reads what an earlier one wrote.
- **end-to-end (e2e)** -- `executor::fold::batch` plus all four of
  `executor::commit`'s flushes (`ram`, `console_out`, `cpu_state`,
  `retention`), the cost a real run actually pays per batch.

Both arms execute the same instruction stream from the same start, and the
run is refused unless they end at the same `pc` and `icount`.

## Warm-up and the compilation regime

ClickHouse compiles an expression DAG once it has executed
`min_count_to_compile_expression` times (3 by default, so the fourth
execution compiles), and counts those executions in a process-static map
that no `SYSTEM` statement resets. `select_only` and `batch` emit the same
step lambda, so two arms sharing a server share one counter and one compiled
function: the first to run pays for the compilation and the second collects
it.

Each arm therefore starts a container of its own, and runs `--warmup`
batches before it times anything. The run is refused unless a warm-up batch
recorded `CompileFunction > 0` and no timed batch did. Both halves are
checked. The second alone would pass on a server that never compiles at all.
Every batch in the output carries `CompileFunction`,
`CompileExpressionsMicroseconds`, its write-log length, its retired count
and why it stopped.

## How it reaches the gameplay window without a multi-hour run

The SQL CPU runs at roughly 1,000-2,000 instr/sec (ADR-0004). Reaching
icount 233,932,753 by live execution would cost tens of hours -- payable
once, not every sprint measurement. `refemu run --dump-state` runs the same ROM
through `refemu` instead (about 170M instr/sec measured), reaching that
icount in under two seconds, and dumps the full CPU state (`pc`, `regs`,
`ram`). `bench canonical` loads that dump directly into an isolated
database's `ram`/`batch_commit`, so the SQL CPU's *first* batch in the
gameplay window starts from real, representative mid-run state -- not a
synthetic fixture, and not a guess at what gameplay state looks like.

The snapshot is cached (`<snapshot-dir>/snapshot.<rom sha256 prefix>.<target
icount>.v<format version>.rsnap`, atomically written) -- generated once per
ROM, reused for every subsequent sprint measurement until `PINNED_HASH`
changes. `--snapshot-dir` says where; it defaults to
`/tmp/clickdoom-canonical-throughput`.

See `refemu::snapshot` for exactly what is and is not captured (short
version: `pc`/`regs`/`ram`/`icount` only -- no framebuffer/palette/console/
MMIO state, since there's no SQL storage for those yet and this benchmark
doesn't need them to measure throughput).

## K, HWM, and the refuse-to-run guarantee

K = 60,000 -- issue #80's analytic optimum (~59,750, flat across
50,000-80,000 once #86's CSE bug was corrected for; see #80's final
comment for the full cost-curve derivation). HWM = 20,000, the SPEC/
production default, used **unchanged**, not inflated to trivially
guarantee no write-log truncation -- raising it would change the very
write-log scan cost this benchmark measures. If a window's real store
density is high enough to trip HWM before K retires at these settings,
`bench canonical` refuses to report a throughput figure computed on a
truncated timed batch (a batch that stops early measures different work than
a full one) -- that refusal is itself the sprint-relevant finding, not
something to route around by quietly raising HWM. A warm-up batch that trips
the mark only has to advance the chain, so it is reported and allowed.

A batch that ends on a FRAME_COMMIT store is neither of those. The batch
execution contract ends a batch there, so it is reported with
`stop=frame_commit` and counted.

## Provenance

Every run prints: ROM sha256, `decoded` row count, the image and the server
version that answered, K, HWM, warm-up and timed batches per arm, and the
git SHA the measurement was taken at.
Every number this project has retracted lost its meaning by being separated
from what produced it.

## Rerunning

    make bench-canonical-throughput

or directly:

    clickdoom bench canonical \
        --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --image clickhouse/clickhouse-server:26.7.5.10 \
        --refemu-bin target/release/refemu

Needs `rom/build/` built (`make build-rom`), `refemu` and `clickdoom` built
(`make build-refemu build-clickdoom`), and Docker. `make up` is not a
prerequisite. Every arm starts and removes a container of its own, and the
Makefile target reads the pinned image out of `docker-compose.yml`.

## What this is not

This does not run, or feed into, a real milestone/demo3 run -- it is a
measurement instrument only, isolated to a `canonical_throughput` database
on a container of its own, never the shared `clickdoom` database. It reports
throughput; interpreting a sprint experiment's result against it is the
sprint's job, not this benchmark's.
