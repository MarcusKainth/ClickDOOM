# Turning short-circuit evaluation off, and the first gameplay baseline

This measures the largest throughput change the project has made since the
`HALT_CODE` binding, and it measures the gameplay window, which no throughput
instrument here had ever reached.

## Question

Does pinning `short_circuit_function_evaluation = 'disable'` make the
production fold faster, and does the answer survive in the window DOOM
actually spends its life in? What does the framebuffer and palette write-log
pair cost where the rasterizer runs? What is a gameplay batch worth?

## Method

Every arm runs the production generator unmodified, one fresh container per
arm, four warm-up batches, then chained timed batches. An arm is refused
unless a warm-up batch compiled and no timed batch did, which both branches of
have live negative controls at K = 60,000.

Arm isolation is a fresh server process rather than
`SYSTEM DROP COMPILED EXPRESSION CACHE`. ClickHouse counts DAG executions in a
process-static map that no `SYSTEM` statement resets
(`src/Interpreters/ExpressionJIT.cpp`), so two query texts sharing one
compilable subexpression share a counter. `select_only` and `batch` emit a
byte-identical step lambda, so on a shared server the first arm arms the JIT
and the second collects it.

Durations come from `system.query_log.query_duration_ms` keyed by the
harness's own `query_id`. The work is verified rather than the query's return:
every batch asserts its retired count, its final `pc` and `icount`, and its
write-log length, and arms are compared on `batch_commit` column by column
plus the `ram`, `framebuffer` and `palette` hashes.

Gameplay is reached from a cached `refemu` snapshot at icount 233,932,753
rather than by executing tens of hours.

## Conditions

| | |
|---|---|
| Date | 2026-09-01 |
| ClickHouse | 26.7.5.10-stable, image digest `sha256:800e8286…fdb6e` |
| ROM | `9a6a47d01119…`, matching `rom/PINNED_HASH` |
| K | 60,000 |
| HWM | 20,000 |
| Settings | `max_threads = 1`, otherwise as generated |
| Machine | Apple Silicon, 18 cores |
| Load | 0.81 to 9.28 of 18 cores, medians 1.79 to 1.91; the merged-main confirmation ran at 1.53 to 3.36 |

**The machine was not quiet.** The owner's Grafana, Loki, Mimir, Tempo,
Postgres, Redis, SeaweedFS and Alloy containers, a long-lived
`clickdoom-ch` server and a macOS Virtualization VM ran throughout, outside
the machine-lock protocol. Every timed comparison is paired inside one
container in one window, so no number here is withdrawn. Absolute figures
carry that condition.

One block broke the serial guarantee. A second block's ClickHouse container
held 108% of a core during the name-term arms and load peaked at 9.28. That
block's absolute figures are excluded and only its paired ratios are used.

## Results

### The baseline, labelled

Boot, steady state, chained batches 5 to 14, four runs on eight fresh
containers.

| | instr/sec | µs per instruction |
|---|---:|---:|
| end to end | 4,586.1 | 218.1 |
| fold alone | 4,608.6 | 217.0 |

Per-run end to end: 4,570.8, 4,609.5, 4,588.7, 4,575.3, standard deviation
17.4, which is 0.38%.

The same series over batches 1 to 3 reads 3,725 end to end and 3,766 fold
alone. That reproduces the 3,760 in
[`native-vs-docker`](native-vs-docker.md), and the two numbers were never in
conflict. They are batches 1 to 3 and batches 5 to 14 of one series. Those
first three batches are simultaneously the only uncompiled batches and, in
boot, the only write-log-saturated ones, so timing them understates steady
state by 18.3%.

The per-batch series, fold alone, mean of four runs: 3797, 3763, 3739, then
4419 on the warm-up where compilation fires, then 4632, 4652, 4666, 4657,
4636, 4608, 4638, 4629, 4618, 4365. Batch 14 is 6.4% low because its write log
has climbed to 12,275. Quoting any single batch is wrong by up to 7%.

### Fold-alone is faster than end to end, and always was

Fold alone measures 0.49% faster than end to end, 95% confidence interval
+0.04% to +0.95% over 40 paired batches, and stays faster with the arm order
reversed.

Every previously recorded run has the opposite sign, which is impossible if
end to end is the fold plus the commit path. The cause is arm order.
Fold-first gives fold 3,738 against batch 3,933; batch-first gives batch 3,673
against fold 3,967. `FORMAT Null` was the suspected cause and is refuted: the
paired difference is 68 ms on a 15,050 ms batch, 0.45%, six times too small,
and `result_bytes` reads 324,299 either way.

### The short-circuit setting

Paired, fresh container per arm, end to end.

| arm | effect |
|---|---|
| guard the divisors, setting inherited | −2.27% ±0.94 |
| guard plus the pin at `disable` | +17.27% ±1.14 |
| net, as the stack lands | +14.62% ±1.87 |

Turning short-circuit evaluation off makes the fold faster. The lazy-column
bookkeeping costs more than the work it skips on a one-row block.

The guard is the price of the setting. At `disable` every arm of every `if`
and `multiIf` evaluates on every step, and before the guard the fold threw
`ILLEGAL_DIVISION` on every program, because `rs2 = x0` makes the divisor zero
on ordinary instructions. The guard costs 4.77 µs per instruction for four
added actions, which is 1.19 µs each, and it buys nothing on its own. The two
land as one unit.

Boot goes from 4,586 to 5,334 instructions per second with the stack.

### Gameplay

The first gameplay throughput figures this project has taken. Twenty-six timed
batches, three repeats, arms alternated, one fresh container per arm per
repeat.

| | instr/sec | µs per instruction |
|---|---:|---:|
| unpinned, end to end | 4,190 | 238.62 |
| pinned, end to end | 4,899 | 204.1 |
| pinned, full-K batches only | 5,088 | 196.6 |

The 5,088 drops the once-per-frame truncated batch and overstates gameplay by
3.8%. These three come from a hand-driven harness; the figure to quote is the
5,060 measured on merged main below.

The pin is worth +17.0% ±0.09 here against +17.27% ±1.14 in boot, so it
survives the window where the framebuffer lanes fire.

A frame is 1,384,418 instructions, range 1,298,343 to 1,509,446 over the last
40 frames, and 24 batches at K = 60,000: 22 plain, one blit, one truncated.
Write logs run 5,308 to 11,272 against the 20,000 high-water mark.

Gameplay is 7.8% slower per instruction than boot on the same pinned fold, and
3.6% slower on full-K batches alone. Almost all of the gap is the truncated
batch rather than the instruction mix.

### The framebuffer and palette lanes

`fold.rs` pushes six accumulator lanes for FRAMEBUFFER and PALETTE stores under
an `if`, which does not short-circuit, so all six run on every instruction.
They carry no high-water mark. In gameplay they reach about 16,000 entries per
batch against boot's steady-state general write log of about 100.

They cost **0.21% to 0.43% of gameplay**. The accumulator-copy rate of 2.673 ns
per element per step predicts about 20 µs per step for six lanes at 16,000
entries. The measurement is far below that.

Giving those lanes their own high-water mark is worse than doing nothing: net
**+11,550 ms per frame** at N = 2. Splitting a batch costs a whole batch rather
than a fixed setup, so the extra batches cost more than the shorter lanes save.

### The truncated batch

Gameplay's once-per-frame truncated batch costs **3.57% of gameplay
throughput**, eight times the lanes, and it is the largest gameplay-specific
cost measured here. There is no candidate fix. A smaller K makes it worse,
because early termination refunds nothing and the unretired tail of `range(K)`
is paid at full price.

### The capture tuple

Changing `groupArray(tuple(value))` to `groupArray(value)` for the RAM and key
queue captures is worth **+1.064%**, 95% confidence interval 0.519 to 1.610,
from a fixed per-batch cost of 87.14 ±5.00 ms fitted over K = 2,000, 10,000 and
60,000 in both modes. The per-step read-site term is 0.62 ±0.80 µs and is not
significant.

Six repeats at K = 60,000 alone cannot resolve it, at ±0.86% against a 1.1%
effect. Amplification at K = 2,000, where the same 87 ms is 9.6% of a batch,
settles it.

The tuple is not a guard. The comment it carried said the combined
`groupArray(tuple(...))` stops `optimize_read_in_order` streaming one column
from physically sorted storage and misaligning it against `word_addr`. Against
1,344 adversarial cases across 84 settings strings and four hostile table
layouts, zero misalignments appear, under a check where `value = word_addr` so
any permutation counts and whose positive controls fire at 6,291,403 of
6,291,456 rows. The decode capture keeps its tuple, because it captures nine
columns and the hazard needs siblings to misalign against.

## Verdict

Pin `short_circuit_function_evaluation = 'disable'`, with every divisor
guarded so it is non-zero for all inputs. It is worth +14.62% ±1.87 end to end
as the stack lands, it holds in gameplay, and it removes a dependency on a
server default rather than pinning around it.

Boot steady state is 5,340 +/- 50 instructions per second and gameplay is
5,060 +/- 50, measured on merged main below. `SPEC.md`'s 5,000 target is met
in both windows, and gameplay clears it by 1.2%, which is close enough that it
should be re-checked rather than assumed after any change.

The framebuffer and palette lanes are not a lever and a high-water mark on
them is a regression. The truncated batch is eight times larger and has no
known fix.

### Confirmed on merged main

Every figure above was taken on branches, arm by arm, before the change stack
merged. This is the same tree a reader can check out, measured by
`clickdoom bench canonical`, five repeats, K = 60,000, HWM = 20,000, four
warm-up then ten timed batches per arm, one fresh container per arm, waiting
for the one-minute load average to fall below 2.0 before each repeat.

| window | mode | instr/sec | us/instruction | sd |
|---|---|---:|---:|---:|
| boot | fold alone | 5,305 +/- 46 | 188.51 | 0.69% |
| boot | end to end | 5,340 +/- 50 | 187.28 | 0.75% |
| gameplay | fold alone | 5,054 +/- 79 | 197.90 | 1.26% |
| gameplay | end to end | 5,060 +/- 50 | 197.62 | 0.79% |

Boot confirms the composed 5,334 to within 0.1%. Gameplay does not confirm the
4,899: it is 3.3% higher. The two were taken by different harnesses, so the
gap is not attributable, and 5,060 is the figure the tree reproduces.

Fold alone and end to end now agree to 0.13% in gameplay and 0.65% in boot,
with overlapping intervals. The end-to-end arm does strictly more work, so the
fold-alone arm carries a small residual cost that the commit path does not
outweigh at this K.

## What went wrong along the way

The lane cost was predicted at about 20 µs per step from the measured
accumulator-copy rate and came in at 0.21% to 0.43% of gameplay. The rate was
measured on the general write log, which is scanned, and applied to lanes that
are never scanned. A rate carries the mechanism it was measured on.

The fold-alone inversion was attributed to `FORMAT Null` before it was
measured, on the reasoning that the select-only query returns three write-log
lanes to the client. That is a real difference and it is 0.45%, against a 5%
to 6% effect. The cause was arm order, and it had been visible in every
recorded run for months as a physically impossible sign.

The same mistake was made a second time on the merged-main run, in the same
direction. A three-repeat run without a cooldown produced a gameplay inversion
of 353 instructions per second, and it was attributed to the fold-alone arm
serialising the framebuffer lanes to the client. Reading `query_duration_ms`
beside the client interval over 280 batches put the two clocks within 0.05% of
each other, so serialisation is not measurable at this size. The inversion
decayed monotonically across those three repeats, -353, -258, -30, and vanished
once the machine was allowed to settle between repeats. It was the box, not
the instrument. A benchmark without a cooldown produces a first repeat that
reads like a finding.

## Limits

Boot covers icount 0 to 840,000 over 14 batches, and gameplay 26 batches from
one snapshot. An earlier session recorded a monotone 5% upward drift over 25
minutes of continuous batching at flat load, larger than batch-to-batch
variance, and nothing here runs long enough to confirm or refute it.

No timed batch anywhere reaches the write-log high-water mark, so the pin's
independence of write-log length at 20,000 entries is a 1.63x extrapolation
from 12,275 on a slope indistinguishable from zero. In this ROM the only
batches at 20,000 are boot's first three, which are also the only uncompiled
ones, so they cannot separate the two effects.

The arm-totality audit that `disable` depends on is a review artefact rather
than a proof. Every arm was checked against 26.7.5.10 and division and array
indexing are the two classes it found. Which functions ClickHouse defers is
decided per function and moves between releases, so a version bump needs the
audit redone.
