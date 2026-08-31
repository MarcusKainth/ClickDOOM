# Write-log growth within a batch

[`batch-attribution.md`](batch-attribution.md) found an interior optimum
near K ≈ 47,900. Fixed per-batch costs alone make larger batches
monotonically better, so an interior optimum implies something in the batch
grows superlinearly with batch length. The fold is 99.86% of a batch, so if
that term is inside the fold it is the largest remaining lever on the
project.

## Question

Does per-instruction cost grow within a batch? If it does, how large is the
term, and what mechanism produces it?

## Method

Write-log length is varied independently of K by seeding the fold's initial
accumulator with `L0` inert entries. Nothing in normal operation can do
that, because the log only grows by retiring stores. K, the starting state,
the query text and the compiled lambda are all held constant and only `L0`
moves, so the slope of fold time against `L0` is read off directly instead
of being fitted out of a three-parameter model.

The slope divided by K is the per-element, per-step cost. A linear scan
predicts it is constant across K, and that constancy, checked at several K,
answers the headline question without depending on any fit and without
instruction mix entering.

The obvious alternative, running single batches of length N from one
checkpoint and fitting `a + bN + cN²`, compares different instruction ranges
of a non-homogeneous window, so a fitted curvature is partly instruction
mix. Seeding sidesteps that: `L0` changes the write-log without changing
which instructions run.

### Why the seed is inert

The fold clamps every word index with `least(…, ram_words - 1)`, for every
address, valid or not. So a real load's word index is always at most
`ram_words - 1` and a seeded address of `UInt32::MAX` can never match. That
is a property of the clamp rather than of the address set this ROM happens
to touch, so it needs no address analysis. The scan is still walked in full,
because `arrayLastIndex` seeks the last match and cannot short-circuit on a
miss.

The structural argument is necessary but not sufficient, so inertness is
also executed as a test, and that test earned its keep on its first run.

### The design that did not survive contact

The first version seeded the accumulator's write-log lanes selectively.
Lanes 1 and 2 are both `Array(UInt32)` and only lane 1 is scanned, so
subtracting a lane-2-only arm from a lane-1-only arm looked like a free way
to split scan cost from copy cost.

It is wrong. The lanes are parallel arrays and a load subscripts the value
lane with an index found in the address lane:

```sql
acc.3.2[arrayLastIndex(z -> z = WA, acc.3.1)]
```

Unequal lane lengths desynchronise them. With the address lane seeded 8 deep
and the value lane empty, a real store lands at address index 9 and value
index 1, and a load that should forward `0x1234` returns `0`. An unequal
seed is not inert and would have timed a different program, silently, with a
plausible-looking number. Only equal-length seeds are valid, so the
scan-and-copy split has to come from standalone primitive rates compared
against the measured slope, which is two instruments sharing no assumption.

### Measurement discipline

- The select-only fold writes nothing, so consecutive arms are independent
  by construction and a repeat is literally re-issuing the query. That is
  checked rather than trusted: `ram`'s active part count must be exactly
  constant and an interleaved control arm must not drift more than 10%.
  Either failing aborts the block.
- `FORMAT Null` on every timed query. The projection includes the three
  write-log lanes, whose serialisation is proportional to `L0`, so without
  this the sweep would partly measure result-set writing.
- The fixed cost is measured per arm rather than assumed flat. A `K = 0`
  probe with the identical seed runs at every `L0` and is subtracted. The
  seed text does grow, by the decimal digits of `L0`, and about 92% of the
  fixed cost is the analyzer walking generated SQL, so "the seed cannot
  affect parse time" is a claim worth checking.
- The compilation regime is made uniform rather than only stated. The sweep
  warms to the compiled regime before the first recorded arm and
  `CompileFunction` is recorded per arm.
- The high-water mark is raised to 200,000 and held constant. The seeded
  `L0` counts toward the mark, so at the production 20,000 every seeded arm
  would trip it on step 1. The mark is baked into the lambda, so holding it
  constant across every arm is what keeps the compiled expression identical.
- An aborted block reports as aborted, with partial output and the reason,
  never a silent retry into a clean-looking result.

## Conditions

| | |
|---|---|
| Date | 2026-08-28 |
| ClickHouse | 26.3.17.4, one container |
| ROM | `9a6a47d01119…`, the pinned hash |
| Window | boot, from reset, no snapshot |
| K | 60,000 for the headline, also 30,000 and 15,000 |
| HWM | 200,000, raised deliberately and held constant |
| Settings | `max_threads = 1` |
| Regime | `CompileFunction = 0` on every arm, uniformly uncompiled |
| Headroom | 12 to 16 idle cores of 18, gated live throughout |
| Baseline | `pc = 0x80000020`, `retired = 60000`, `halted = 0`, `wl_len = 19998` |

## Results

### The slope

Boot window, K = 60,000, 5 repeats, median net fold time with the `K = 0`
probe subtracted.

| L0 | median net ms | residual against the fit |
|---:|---:|---:|
| 0 | 25,383 | +6.1 |
| 5,000 | 26,394 | -6.1 |
| 10,000 | 27,343 | -80.3 |
| 20,000 | 29,848 | +378.3 |
| 40,000 | 33,647 | +84.5 |
| 80,000 | 41,742 | -6.1 |

```
Theil-Sen slope : 0.204640 ms per seeded element
95% bootstrap CI: [0.196243, 0.216750]
OLS slope       : 0.204618   R^2 = 0.99930
per element per step (slope/K): 3.4107 ns
```

At `L0` = 80,000 the batch is 1.64x its unseeded self.

### The slope divided by K

A linear scan predicts per-element cost is constant per step, so the slope
divided by K must not move with K. Boot window, 5 repeats per point, quiet
machine.

| K | ns per element per step |
|---:|---:|
| 15,000 | 3.5133 |
| 30,000 | 3.2748 |
| 60,000 | 3.4107 |

A 7.0% spread across a 4x range of K. This is the answer to the original
question and it rests on no model.

### Reconciliation with the fitted term

The per-batch fit priced a whole 120,000-instruction window; this prices one
element. The only comparable unit is nanoseconds per element per step.
Converting the fitted `β·W = 0.0916 ms` needs the window's store density,
because its log grows from 0 to `ρ·K` while a seeded log is constant:

```
cost per (step * element) = 2 * beta*W / (W * rho)
```

which has no K in it, and that invariance is measured here directly. With
`ρ = 20,000/60,006 = 0.3333`:

| source | ns per element per step |
|---|---:|
| measured here | 3.411, 95% CI [3.271, 3.612] |
| implied by the fitted term | 4.580, a ratio of 0.74x |

Two instruments sharing no assumption agree within a factor of 0.74.

### How many scans the text asks for

Verified against the generated expression:

```
$ python3 -c "import sys; sys.path.insert(0,'executor'); import fold
s = fold.build_step(60000, 0, 98824, 98824, 6291456, hwm=20000)
print(len(s), s.count('arrayLastIndex'), s.count('arrayPushBack(acc.3'))"
57006 6 3
```

The load word carries two `arrayLastIndex` calls and is textually expanded
three times, so the step expression asks for six scans per step, not two.
There is also a second length-proportional cost: the three `arrayPushBack`
calls on the write-log lanes run on every step whether or not a store
retires, because `if` does not short-circuit inside `arrayFold`.

### The measured cost is far below what the text implies

Standalone primitive rates, each gated on a linearity check, because a
microbenchmark whose work was optimised away reads exactly like a fast one.
The scan costs 2.790 ns per element (r² = 0.999) and the three-lane copy
0.633 ns per element.

| if the fold paid… | predicted ns | measured / predicted |
|---|---:|---:|
| 6 scans plus copy, the naive text count | 35.225 | 0.10x |
| 1 scan plus copy | 21.274 | 0.16x |
| 6 scans, no copy | 16.741 | 0.20x |
| 1 scan, no copy | 2.790 | 1.22x |

Only the last row is close, which points at ClickHouse collapsing the six
textually repeated calls into one.

### The mechanism, observed rather than inferred

Comparing the slope against primitive rates can only establish that the fold
is cheaper than a naive reading of its text implies. It cannot say which of
two candidate reasons is responsible, because both produce the same
shortfall. Two probes test each directly, outside the fold.

The first asks whether `arrayPushBack` copies the accumulator.
`arrayFold((acc, i) -> arrayPushBack(acc, i), range(N), [])` is quadratic in
N if the accumulator is copied each step and linear if it is mutated in
place, so the growth exponent answers it. Grown with the write-log's exact
three-lane shape inside the fold, where the copies are the work and the
final `length(...)` is constant time:

```
N=40,000   778 ms      N=160,000   8,264 ms
N=80,000 2,304 ms      N=320,000  32,419 ms
```

The exponent reads 1.97, so the accumulator is copied every step.

The second asks whether ClickHouse collapses the repeated scan. Two folds
over identical data, one evaluating `arrayLastIndex` once per step and one
evaluating the byte-identical call six times, cost the same to within 0.98x.
Common subexpression elimination fires, and the fold pays for one scan.

Without those probes the mechanism would have been an inference from three
consistent measurements, and the inference would have been half wrong: the
best fit before the probes ran was "one scan, no copy", and the copy is real
and merely cheap.

### The completed picture

| term | ns per write-log element per step | share |
|---|---:|---:|
| load-forwarding scan, once after dedup | 2.671 | 81% |
| write-log three-lane copy | 0.633 | 19% |
| predicted total | 3.304 | |
| measured | 3.411 | 1.03x |

Three independent instruments agree to within 3%.

## Verdict

Per-instruction cost does grow within a batch, linearly in write-log length,
at 3.41 ns per element per step with a 95% confidence interval of 3.27 to
3.61 ns. The earlier fitted term stands, and its own caveat that three
parameters from three points would fit anything is answered by a direct
measurement with an interval that reconciles to 0.74x from an instrument
sharing none of its assumptions.

Binding the load-forwarding scan once has no headroom. ClickHouse already
collapses all six textual calls, so doing it by hand recovers nothing.

The remaining lever is the scan itself, and it is bounded. The scan is 81%
of the write-log term, and the write-log term is 10.3% of a K = 60,000 boot
batch, so the whole linear-scan mechanism is about 8% of a batch. Any
structure that removes the scan, a map, an index or a sorted log, is
competing for that 8%. That is small enough to argue the line of work should
stop.

## What went wrong along the way

Both mistakes are easy to repeat and are recorded for that reason.

The copy rate was overstated by about 30x by a microbenchmark that built the
three lanes outside a fold and forced materialisation with `cityHash64` over
the pushed arrays. The hash is itself proportional to length and dominated
the measurement, reporting 18.5 ns per element where the real copy is 0.633.
Scaffolding has to be cheaper than the thing it scaffolds.

The growth exponent was read too early. At N up to 80,000 it reads about
1.43, which is ambiguous between linear and quadratic; it only reaches 1.97
by N = 320,000. An exponent has to be read where the asymptote is, not where
the sweep happens to stop.

The live headroom gate also earned its keep. One block aborted mid-run at
6.1 idle cores against a floor of 8.0, with load at 10.51 on 18 cores,
caused by macOS `appinstalld` at 265% CPU, `syspolicyd` at 100%, and
XProtect scanning. A start-only check would not have caught it, because the
machine was quiet when the run began. The contaminated arms were discarded
and re-run rather than reported.

## Limits

`CompileFunction` reads 0 on every arm despite four warm-ups, whereas the
per-batch attribution measurement saw compilation fire on the 4th batch. The
regime is at least uniform, which is what the slope needs, but the
discrepancy is unexplained and matters before anyone compares absolute batch
times across the two.

The gameplay window is not measured. Its general-RAM store density is about
9.4% against boot's about 33%, because framebuffer and palette stores go to
an accumulator lane that is not scanned, so the predicted gameplay term is
about 0.28x boot's. That is the opposite of what a store-density argument
predicts without knowing about the separate lane.
