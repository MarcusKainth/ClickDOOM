# Per-statement attribution of one batch

This times every statement an end-to-end batch issues, separately, against
the real ROM. It also sweeps K over a fixed instruction window, varying only
how many batches that window is cut into.

## Question

Where does the time in one end-to-end batch go, statement by statement? Is
the fold's per-batch setup a fixed cost that a larger K would amortise?

## Method

Nothing here reimplements what it measures. The fold SQL comes from the
production generator, the four flushes from the production commit
generators, and the ROM load, decode and bootstrap from the production
scripts. Every statement is issued with its own `query_id` and its duration
read back from `system.query_log` afterwards, because the fold's SQL is
byte-identical across batches and so query text is not a usable key.

Five pieces of measurement discipline hold the numbers up.

- A fresh container per arm rather than a fresh database. ClickHouse's
  compiled-expression cache is server-global and keyed by each island's
  DAG. The step expression is byte-identical at every K, 55,295 characters
  at both K = 2,000 and K = 60,000, because K only reaches `range(K)`,
  outside the lambda. One server would share a single cache key across an
  entire K-sweep and the first arm would warm it for all the others.
- A headroom check as well as container separation. A private container
  isolates ClickHouse state and not CPU, so each arm reads the load average
  against the core count before touching anything and prints the idle-core
  count next to the numbers it qualifies.
- The compilation regime is reported rather than smoothed away.
  `min_count_to_compile_expression` defaults to 3, so compilation lands on
  the 4th execution of a DAG. `CompileFunction` and
  `CompileExpressionsMicroseconds` are read back for every fold, so a number
  always carries the regime it was taken in.
- The work is verified, and not only the query's return. The RAM-capture
  timing wraps the `groupArray` in `length(...)` and asserts the result
  equals the table's row count, so a materialised 6.29M-element array is
  proven rather than assumed. After the batches, the row count must be
  checked with `FINAL`: every batch's flush appends a part, so the raw count
  grows with the number of stores while the deduplicated count stays at
  6,291,456, and checking the raw count fails on a correct run.
- No ratio from a single noisy pair. The sweep holds the executed
  instruction window fixed and varies only the batch count, so each adjacent
  arm pair gives an independent estimate of the per-batch setup, and their
  spread is the error bar. A sweep of one batch each at K = 30,000, 60,000
  and 120,000 would instead have compared three different instruction ranges
  of a non-homogeneous window and read instruction mix as slope.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ClickHouse | 26.3.17.4, fresh private container per arm |
| ROM | `eabb12ed…b92c`, the pinned hash at the time |
| Window | boot, from reset |
| K | 60,000 |
| HWM | 20,000 |
| `ram` | 6,291,456 rows dense; `decoded` 98,824 rows |
| Headroom | 13.9 to 15.5 idle cores of 18, recorded per arm |
| Durations | server-side `query_duration_ms`, keyed by `query_id` |

## Results

### Where a batch's time goes

Four chained end-to-end batches, 60,000 of 60,000 retired on each, no
high-water-mark truncation.

| statement | b0 | b1 | b2 | b3 | mean | share of batch |
|---|---:|---:|---:|---:|---:|---:|
| fold | 28,100 | 29,166 | 28,997 | 24,762 | 27,756 ms | 99.86% |
| `ram` flush | 4 | 4 | 4 | 3 | 3.75 ms | 0.013% |
| `console_out` flush | 2 | 2 | 2 | 7 | 3.25 ms | 0.012% |
| `cpu_state` flush | 2 | 2 | 2 | 2 | 2.00 ms | 0.007% |
| retention DELETE | 10 | 23 | 10 | 16 | 14.75 ms | 0.053% |
| total | | | | | 27,780 ms | |

Client-side wall clock for each flush is 70 ms to 90 ms, but about 65 ms of
that is `docker exec` plus `clickhouse-client` process startup, which a
driver holding one connection does not pay. Taking even the inflated client
number at face value, retention is 0.28% of the batch.

b3 is faster than b0 to b2 because compilation lands on the 4th execution:
`CompileFunction = 63`, 261 ms of LLVM.

### The retention mutation

From `system.part_log`:

| arm | `batch_commit` parts | MutatePart events | avg size | total duration |
|---|---|---:|---:|---:|
| 4 batches | Compact, 5 of 5 | 14 for 4 DELETEs | 106.7 KB | 48 ms |
| 20 batches | Compact, 21 of 21 | 71 for 20 DELETEs | 162.2 KB | 278 ms |

More MutatePart events than DELETE statements confirms that the mutation
touches every active part, not only the parts holding deleted rows. The
whole background cost of that is 12 ms of CPU per batch against a batch of
about 27,800 ms.

Retention is 0.053% of the batch cold and 0.13% in steady state, where a
steady-state batch is about 25,300 ms. Batch-to-batch fold variance in the
compiled regime is 705 ms, 2.8% over 17 batches, so an end-to-end arm cannot
resolve a 34 ms effect: it is 20x below the noise floor. A retention change
should be argued from `system.part_log`'s `read_bytes`, not from an
end-to-end timing.

### The fold's fixed setup, measured directly

The select-only fold over `range(0)` parses and analyses the identical
58 KB, roughly 90,000-node query, evaluates all three `WITH` captures, and
runs the step lambda zero times. Its duration is the fixed setup. The
harness asserts that 0 instructions retired.

```
select_only(K=0):  1647 / 1674 / 1665 ms   (retired = 0, verified)
                   1602 / 1654 / 1638 ms   (second arm, different container)
select_only(K=1):  1918 / 1674 / 1662 ms   (retired = 1)
K=1 full batches:  1649 / 1659 / 1694 / 1742 / 1720 ms
```

Fixed setup is about 1,650 ms. The K = 1 numbers agree with the K = 0
numbers to within noise, so one step costs about 0.4 ms.

Each capture timed standalone with the same SQL text the fold uses, at the
same `max_threads = 1`, wrapped in `length(...)` and asserted against the
table's row count:

| component | duration | share of setup |
|---|---:|---:|
| RAM capture, 6.29M-row `ReplacingMergeTree` `FINAL` with `ORDER BY` into a `groupArray` | 118 ms | 7% |
| decode capture, 98,824-row `groupArray` of 9-tuples | 9 ms | 0.5% |
| key-queue capture, empty | 1 ms | 0.1% |
| parse, analyse and plan the generated expression | about 1,530 ms | 92% |

The RAM capture, which had been the suspected cause, is 0.4% of a 26.7 s
batch. The fixed cost is the analyzer walking generated SQL.

Read standalone on a different arm at 6 active parts, the RAM capture
measures 132, 131 and 132 ms, against the 118 ms in the decomposition above.

### The fixed-window sweep

Every arm executes the identical 120,000 `range()` iterations from the
identical reset state, differing only in how many batches they are cut into.

| K | batches | fold total | retired | ms per `range()` iteration |
|---:|---:|---:|---:|---:|
| 15,000 | 8 | 59,760 ms | 120,000 | 0.4980 |
| 30,000 | 4 | 54,119 ms | 120,000 | 0.4510 |
| 60,000 | 2 | 53,359 ms | 120,000 | 0.4447 |
| 120,000 | 1 | 53,963 ms | 60,006 | 0.4497 |

K = 60,000 is the fastest arm. K = 120,000 took longer than K = 60,000
despite paying one setup instead of two, which is the opposite of what a
model of the form `setup + per-step cost times K` predicts. Fitting the
setup from adjacent pairs under that model gives 1,410 ms from the 8-batch
and 4-batch arms and 380 ms from the 4-batch and 2-batch arms. Two
independent estimates of the same constant differing by 3.7x is the model
being wrong rather than noise.

### The model that fits

Load forwarding scans the whole write log linearly on every load, and the
write log grows within a batch, so per-step cost grows within a batch. That
makes total cost superlinear in K, which pushes against setup amortisation.
Fitting `Total(K) = (W/K)·S + a·W + β·W·K` over the fixed window
W = 120,000:

| parameter | value |
|---|---|
| `S` | 1,754 ms |
| `a` | 0.3696 ms per step |
| `β·W` | 0.0916 ms per unit of K |

| K | predicted | observed | residual |
|---:|---:|---:|---:|
| 15,000 | 59,756 | 59,760 | 4 ms |
| 30,000 | 54,115 | 54,119 | 4 ms |
| 60,000 | 53,355 | 53,359 | 4 ms |
| 120,000 | 57,098 | 53,963 | -3,135 ms |

Three parameters from three points is exactly determined and would fit
anything. What makes it credible is that `S` falls out at 1,754 ms when a
separate probe reads it directly at 1,650 ms. The K = 120,000 residual is
write-log growth saturating at the high-water mark.

Breaking a K = 60,000 batch down under that fit: setup 1,754 ms (6.6%),
linear step work 22,176 ms (83.1%), write-log growth 2,747 ms (10.3%). The
optimum is K ≈ 47,900, at 53,133 ms against 53,359 ms at K = 60,000, a
difference of 0.4%.

Setup is the one term here that moves with the ClickHouse version. On
26.7.5.10 the same `K = 0` probe against the real production batch reads
624 ms, 4.8% of a 13,088 ms steady-state batch, so it shrank about 2.6x
where the batch shrank the 1.77x [`native-vs-docker.md`](native-vs-docker.md)
measures. Of that 624 ms, 117 ms is the RAM capture, and 91 ms of the 117 is
its `groupArray(tuple(value))` wrapper, so a capture spelled without the
tuple puts setup at 531 ms.

### K is capped by the high-water mark

Measured directly. Boot window, K = 80,000, one batch:

```
retired = 60,006   halted = 0   length(wl_addr) = 20,000
```

The fold stopped on the high-water mark at instruction 60,006, then iterated
the remaining 19,994 `range()` elements as no-ops at full price. The same
happens at K = 120,000, where 59,994 iterations are wasted and the arm does
half the useful work of the K = 60,000 arm in the same wall time. K = 60,000
sits six instructions below a hard cap.

The gameplay window is not the binding case, which is the opposite of what
store density suggests. Seeded from a reference-emulator snapshot at icount
233,932,753, K = 60,000, three batches retired 60,000 each with write-log
lengths of 5,672, 6,163 and 4,928. Framebuffer and palette stores go to
their own accumulator lane, which does not count toward the write-log
high-water mark, and gameplay's rasterizer stores are overwhelmingly
framebuffer stores.

### No degradation over 20 batches

| probe | before batches | after batches |
|---|---|---|
| RAM capture, gameplay, 3 batches, 6 to 8 active parts | 118 / 122 / 127 ms | 118 / 127 ms |
| RAM capture, boot, 1 batch, 6 to 3 active parts after merge | 115 / 116 / 129 ms | 106 / 109 / 105 ms |

The fold itself over 20 chained batches at K = 60,000: batches 3 to 19 mean
25,051 ms with a standard deviation of 705 ms (2.8%), range 24,128 to
27,072 ms, no trend. `ram` accumulated 20 new parts and background merges
kept up with 9 MergeParts events across the run.

The drop at the 4th batch is about 16% here, 29,661 ms down to 25,051 ms,
and it stays down afterwards. Compilation is not all of it. The same drop
reproduces on 26.7.5.10 as 17.0% between batches 1 to 3 and batches 5 to 14
of the boot series in [`compiled-node-cost.md`](compiled-node-cost.md), and
it splits in two. Compilation is worth 5.8% to 8.1%, measured paired inside
one container at three work points with the work verified byte-identical.
The other term is the write log falling away from the 20,000 high-water mark
as boot leaves its memset loop, about 11%, which
[`write-log-growth.md`](write-log-growth.md)'s slope predicts at 2,046 ms
against 2,012 ms measured. Reading the whole drop as compilation overstates
compilation by about 2.7x.

The first batches of a boot run are the only ones that are both uncompiled
and holding the write log at the mark, so a three-batch benchmark measures
the wrong regime for a multi-day run and measures two terms at once.

## Verdict

A batch is 99.86% fold. The entire commit path plus the fold's fixed setup
is about 1%, so there is no lever in the commit path.

Raising K is rejected. The fixed setup is real at about 1,650 ms, 6.6% of a
K = 60,000 batch on this version and 4.8% of one on 26.7.5.10, but it is the
analyzer walking generated SQL rather than the RAM capture, larger K does not
amortise it usefully because write-log growth is superlinear, and in the boot
window larger K is not available at all. The optimum is 0.4% from the K in
use.

A retention change is worth taking if it is free and mechanically right. Do
not expect a throughput number from one: retention is 0.13% of a
steady-state batch.

The proposal this left behind, to bind the load-forwarding scan once instead
of letting it appear repeatedly in the generated text, was measured
afterwards and has no headroom: ClickHouse already collapses the repeated
calls. [`write-log-growth.md`](write-log-growth.md) is that measurement, and
it also puts a confidence interval on the write-log term fitted here.

## Limits

Degradation is not visible at 20 batches. It is not settled at a full
`demo3` run, which is tens of thousands of batches. The component that would
degrade is 0.4% of a batch, so the concern is much smaller than it looks.
