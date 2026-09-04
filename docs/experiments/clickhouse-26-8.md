# The two modes on ClickHouse 26.8.2.7

The pin moves from 26.7.5.10 to 26.8.2.7. Emulation and native mode read
differently on it, so both are measured here, the same way on both versions.

## Question

What do the canonical throughput windows, and the resident simulation
statement, read on 26.8.2.7 against 26.7.5.10?

## Method

`main` at `d465403`. Two image digests, both from `docker-compose.yml`'s own
form: 26.7.5.10 at `sha256:800e82865530` and 26.8.2.7 at
`sha256:fa394da808cc`. The machine lock was held for every timed run and
released between runs.

Emulation is `clickdoom emulation bench compare-versions`, K = 60,000,
HWM = 20,000, three repeats of three timed batches, arm order rotated per
repeat, one fresh container per arm. Both arms of every repeat end at the same
pc and icount, and every timed batch retired the full K.

Native is three rounds per version, interleaved, on two throwaway containers of
this lane's own with `docker/clickhouse/config.d` and `users.d` mounted as the
compose file does. The instrument opens the resident simulation statement
through `driver`'s own `Session::open` the way `native diff` does, feeds tics 1
to 124, and reads `FunctionExecute` from `system.processes` at each window
boundary. Idle is gametics 2 to 61 and chasing is gametics 108 to 124. The
statement is 1,069,919 bytes for a database called `clickdoom_native`.
`QueryAnalysisMicroseconds` is that statement's row in `system.query_log`.

The machine was not quiet. Load average was 5.28 at the start, with idle
ClickHouse containers from other lanes and an observability stack running
throughout. A batch of K = 60,000 took 17 to 18.4 s against the 12 s
[`frame-200-window`](frame-200-window.md) records on a quiet machine, so the
absolute emulation rates read about 25% low and the version ratio is the figure
to read.

## Numbers

Emulation, instructions per second summed over the three repeats:

| window | mode | 26.7.5.10 | 26.8.2.7 | ratio |
|---|---|---|---|---|
| boot, from icount 0 | fold-alone | 4,763.0 | 4,425.7 | 0.93x |
| boot, from icount 0 | end to end | 4,813.3 | 4,669.2 | 0.97x |
| gameplay, from frame 200 | fold-alone | 4,411.7 | 3,951.2 | 0.90x |
| gameplay, from frame 200 | end to end | 4,346.6 | 3,834.0 | 0.88x |

The gameplay end-to-end spread is wide on the older version: 26.7.5.10 reads
3,796.4, 4,740.6 and 4,633.1 over the three repeats and 26.8.2.7 reads 3,917.7,
3,858.1 and 3,731.0. The fold-alone comparison is the tighter one, and there
26.8.2.7 is slower in every repeat.

Native, three rounds and the minimum of each:

| figure | 26.7.5.10 | 26.8.2.7 | ratio on min |
|---|---|---|---|
| analysis | 78.505, 66.761, 72.253 s | 21.773, 22.739, 23.923 s | 3.07x |
| first tic, wall | 80.279, 68.541, 73.698 s | 23.519, 24.548, 25.373 s | 2.91x |
| idle wall per tic | 29.49, 25.90, 30.02 ms | 21.96, 20.90, 22.98 ms | 1.24x |
| chasing wall per tic | 30.35, 29.94, 31.53 ms | 23.96, 25.14, 25.84 ms | 1.25x |
| peak memory | 429.91, 430.92, 429.94 MiB | 427.44, 428.38, 427.41 MiB | 1.01x |

`FunctionExecute` per tic is identical between the two versions in every round,
which is what says both did the same work: idle reads 16,454, 15,530 and 15,316
and chasing reads 16,646, 16,006 and 15,780 on both.

`clickdoom native demo demo3 --stop-at-frame 300 --no-window` renders from the
probe's state rows, which is the only mode that command offers, so these are
renderer figures and no simulation runs in them. 26.7.5.10 reads 34.4, 34.4 and
34.2 frames per second with 23.7, 23.9 and 23.7 ms of render per frame;
26.8.2.7 reads 34.9, 34.8 and 34.8 with 20.8, 22.0 and 22.1 ms.

Correctness on 26.8.2.7:

| check | result |
|---|---|
| `native diff 300 --probe <trace>` | exit 3 on both versions, `tic 146 player slot 0 p_extralight: 1 against the probe's 2` |
| 300-tic state rows, `hex(cityHash64(groupArray(cityHash64(*))))` | `8DB4380B02278470` on both versions |
| `make native-smoke` | exit 0 |
| `clickdoom emulation diff 100000` | exit 0, 24 register checkpoints, no divergence |

## Verdict

Emulation is slower on 26.8.2.7 and native is faster. The gameplay window costs
0.88x end to end and 0.90x fold-alone. The resident simulation statement's
analysis costs 3.07x less and its tic 1.24x less, with the same
`FunctionExecute` count on both versions.

The analysis figure is what the first-tic budget in
`driver/src/cli/native/diff.rs` is sized against. On 26.7.5.10 the first tic
takes 68.5 to 80.3 s here; on 26.8.2.7 it takes 23.5 to 25.4 s.

Measured 4 September 2026.
