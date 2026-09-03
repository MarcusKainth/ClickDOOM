# Canonical real-ROM throughput benchmark

The instrument every throughput claim about the SQL CPU comes from. Numbers
taken by different harnesses, at different K, against different ROMs cannot be
compared with each other, so this is the one that reports them.

## What it measures

Two windows against the frozen ROM (`rom/PINNED_HASH`), picked from the
memory-function profile's per-symbol attribution:

| window | starts at | what runs there |
|---|---|---|
| boot | icount 0 | WAD load, `R_Init*`, `strncasecmp`-heavy lump lookups, read and scan dominated |
| store-heavy gameplay | icount 194,583,691 | `-timedemo demo3` playback, `R_DrawColumn` and `R_DrawSpan` dominating, both pixel-store-bound rasterizers |

Each window's label names the frame it covers, so a measurement says which
game state it was taken in.

One blended whole-run average would hide the effect the two windows exist to
separate: a correctness check that costs more on memory-heavy code than on
the instruction stream generally compounds in the rasterizer rather than
diluting evenly. The two are reported separately so that shows.

Each window is measured two ways, reported separately:

- **fold-alone**: `executor::fold::select_only`, the cost of the `arrayFold`
  step expression itself. Its write logs are applied to
  `ram`/`framebuffer`/`palette` between chained batches, outside the timed
  statement, so a later batch reads what an earlier one wrote.
- **end-to-end**: `executor::fold::batch` plus all four of
  `executor::commit`'s flushes (`ram`, `console_out`, `cpu_state`,
  `retention`), the cost a real run pays per batch.

Both arms execute the same instruction stream from the same start, and the
run is refused unless they end at the same `pc` and `icount`.

Alongside instructions per second, each arm reports seconds to first frame:
the ROM's instructions to first frame, measured by `refemu` in the same run,
divided by that arm's rate. A ROM change that retires fewer instructions for
the same frame moves it and leaves instructions per second alone.

## Where the gameplay window starts, and how long it is

The window starts on the instruction after frame 200's `FRAME_COMMIT` store
retires. `refemu run --stop-at frame:200` reports that icount, and the run is
refused unless it is still 194,583,691:

    refemu run rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --pinned-hash rom/PINNED_HASH --stop-at frame:200 --halt-report -

Starting on a frame commit is what makes the window long. The batch execution
contract ends a batch on a `FRAME_COMMIT` store, and `arrayFold` runs K steps
whether or not they retire, so a batch cut by a frame commit is charged the
full K against the fewer instructions it retired. Frame 201 commits at icount
195,961,602, which leaves 1,377,911 instructions, or 22 whole batches at
K = 60,000. The default four warm-up plus three timed batches per arm need
seven. The preflight checks that count against the measured span and refuses
the run rather than reporting a rate taken over a truncated batch.

The write log has room over the same span. Counting the RAM stores in each
60,000-instruction batch of the window with

    refemu run rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --pinned-hash rom/PINNED_HASH --resume <frame-200 capture> \
        --watch-from icount:<batch start> --stop-at icount:<batch end> \
        --watch-writes ram --write-coverage -

gives 5,673 in the first batch and 12,962 in the heaviest of the 22, against
a high-water mark of 20,000 (`CLICKDOOM_RUN_HWM`). Framebuffer and palette
stores go to their own accumulator lanes, which the mark does not count, and
gameplay's rasterizer stores are overwhelmingly framebuffer stores.
[`batch-attribution.md`](../../../docs/experiments/batch-attribution.md)
measures the same asymmetry from the other side: boot saturates the mark and
gameplay does not.

## How it reaches the gameplay window without a multi-hour run

At the gameplay rate [`docs/benchmarks.md`](../../../docs/benchmarks.md)
indexes, executing to icount 194,583,691 through the SQL CPU would cost about
eleven hours. `refemu run --dump-state` runs the same ROM through the
reference emulator instead, reaching it in seconds, and writes the whole
machine out. `bench canonical` loads that capture straight into an isolated
database's `ram`/`framebuffer`/`palette`/`batch_commit`, so the SQL CPU's
first batch in the window starts from real mid-run state rather than a
synthetic fixture.

The capture is cached at `<snapshot-dir>/snapshot.<rom sha256
prefix>.<target icount>.v<format version>.rsnap` and written atomically, so
it is generated once per ROM and reused until `rom/PINNED_HASH` moves.
`--snapshot-dir` says where; it defaults to
`/tmp/clickdoom-canonical-throughput`.

`refemu::snapshot` states what a machine capture holds: `pc`, `regs`,
`icount`, `ram`, `framebuffer` and `palette`. Console bytes and MMIO device
state are not captured, and throughput does not depend on them.

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

## What the run refuses to report

K = 60,000 and HWM = 20,000 are the values `make bench-canonical-throughput`
passes, from `CLICKDOOM_RUN_K` and `CLICKDOOM_RUN_HWM`. HWM is the
production default, used unchanged: raising it to guarantee no truncation
would change the write-log scan cost this benchmark measures.

A timed batch that retires fewer than K measures different work than a full
one, because `arrayFold` runs K steps either way. Both contract conditions
that can cut one short are refused rather than averaged in:

- the write log reaching the high-water mark, which the boot window's memset
  loop does on its first batches;
- a `FRAME_COMMIT` store, which the gameplay window's span check exists to
  keep outside every batch.

A warm-up batch only has to advance the chain, so either stop is reported and
allowed there. A halt in any batch ends the run, because a window that halts
partway has no throughput to report.

## Provenance

Every run prints: ROM sha256, `decoded` row count, the image and the server
version that answered, K, HWM, warm-up and timed batches per arm, the ROM's
instructions to first frame, and the git SHA the measurement was taken at.
Every number this project has retracted lost its meaning by being separated
from what produced it.

## Rerunning

    make bench-canonical-throughput

or directly:

    clickdoom emulation bench canonical \
        --bin rom/build/doom-rv32im.bin --manifest rom/build/manifest.json \
        --image clickhouse/clickhouse-server:26.7.5.10 \
        --refemu-bin target/release/refemu

Needs `rom/build/` built (`make build-rom`), `refemu` and `clickdoom` built
(`make build-refemu build-clickdoom`), and Docker. `make up` is not a
prerequisite. Every arm starts and removes a container of its own, and the
Makefile target reads the pinned image out of `docker-compose.yml`.

## What this is not

This does not run, or feed into, a real milestone or demo3 run. It is a
measurement instrument, isolated to a `canonical_throughput` database on a
container of its own, never the shared `clickdoom` database. It reports
throughput; what a result means for a change is the change's own argument.
