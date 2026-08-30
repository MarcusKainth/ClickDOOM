# Canonical real-ROM throughput benchmark

## Why this exists

The human owner ruled on #130's 1.61x throughput regression (recorded in
#147): the Phase 0 "1,000 instr/sec floor" was an architecture-viability
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

Two windows, identified from `rom/bench/e7_memfns`'s exact per-symbol
attribution against the current frozen ROM (`rom/PINNED_HASH`):

| window | icount range | what's happening |
|---|---|---|
| boot-phase | `[0, 15,393,136)` | WAD load, `R_Init*`, `strncasecmp`-heavy lump lookups -- read/scan-dominated |
| store-heavy gameplay | `[233,932,753, 392,488,489)` | frames 200->299 of real `-timedemo demo3` playback -- `R_DrawColumn`/`R_DrawSpan` dominate, both pixel-store-bound rasterizers |

One blended whole-run average would hide exactly the effect `executor`
found in #130: added correctness checks compound on memory-heavy code
rather than diluting evenly across the instruction stream. The two windows
are reported separately so that effect is visible instead of averaged away.

Each window is measured two ways, reported separately (ADR-0004's own
convention):

- **fold-alone** -- `executor/fold.py`'s `select_only()`, the cost of the
  `arrayFold` step expression itself.
- **end-to-end (e2e)** -- `fold.py`'s `batch()` plus all four of
  `executor/commit.py`'s flushes (`ram`, `console_out`, `cpu_state`,
  `retention`), the cost a real run actually pays per batch.

## How it reaches the gameplay window without a multi-hour run

The SQL CPU runs at roughly 1,000-2,000 instr/sec (ADR-0004). Reaching
icount 233,932,753 by live execution would cost tens of hours -- payable
once, not every sprint measurement. `gen_snapshot.py` runs the same ROM
through `refemu` instead (~0.9-1.0M instr/sec measured), reaching that
icount in about four minutes, and dumps the full CPU state (`pc`, `regs`,
`ram`). `seed_snapshot.py` loads that dump directly into an isolated
database's `ram`/`batch_commit`, so the SQL CPU's *first* batch in the
gameplay window starts from real, representative mid-run state -- not a
synthetic fixture, and not a guess at what gameplay state looks like.

The snapshot is cached (`<snapshot-dir>/snapshot.<rom sha256 prefix>.<target
icount>.pkl`, atomically written) -- generated once per ROM, reused for
every subsequent sprint measurement until `PINNED_HASH` changes.

See `gen_snapshot.py`'s own docstring for exactly what is and is not
captured (short version: `pc`/`regs`/`ram`/`icount` only -- no
framebuffer/palette/console/MMIO state, since there's no SQL storage for
those yet and this benchmark doesn't need them to measure throughput).

## K, HWM, and the refuse-to-run guarantee

K = 60,000 -- issue #80's analytic optimum (~59,750, flat across
50,000-80,000 once #86's CSE bug was corrected for; see #80's final
comment for the full cost-curve derivation). HWM = 20,000, the SPEC/
production default, used **unchanged**, not inflated to trivially
guarantee no write-log truncation -- raising it would change the very
write-log scan cost this benchmark measures. If the gameplay window's real
store density is high enough to trip HWM before K retires at these
settings, `run.sh` refuses to report a throughput figure computed on a
truncated batch (a batch that stops early measures different work than a
full one) -- that refusal is itself the sprint-relevant finding, not
something to route around by quietly raising HWM.

## Contention detection

Checked once before starting and once between windows: `docker stats`
against the shared container plus host load average, both point-in-time
checks. Aborts rather than caveats -- see `run.sh`'s own comment for what
this does and does not catch (it cannot detect contention that begins
mid-window; `arrayFold` doesn't yield control back to check).

## Provenance

Every run prints: ROM sha256, `decoded` row count, K, HWM, batches per
mode, and the git SHA the measurement was taken at. Every number this
project has retracted lost its meaning by being separated from what
produced it.

## Rerunning

    make bench-canonical-throughput

or directly:

    rom/bench/canonical_throughput/run.sh \
        --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json

Needs `rom/build/` built (`make build-rom`) and the pinned ClickHouse up
(`make up`). Coordinate with whoever else might be using the shared
container first -- the contention check catches an already-busy container
at start time, but not a teammate who starts a run seconds later.

## What this is not

This does not run, or feed into, a real milestone/demo3 run -- it is a
measurement instrument only, isolated to its own throwaway databases
(`canonical_throughput_boot_<pid>`, `canonical_throughput_gameplay_<pid>`,
dropped on exit), never the shared `clickdoom` database. It reports
throughput; interpreting a sprint experiment's result against it is the
sprint's job, not this script's.
