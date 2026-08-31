# Benchmarks

Every experiment this project has run, what it settles, and what came out. A
finding nobody can find is not evidence.

[`experiments/`](experiments/) holds one record per experiment. Each carries
the question it settles, how it was measured, the numbers with the
ClickHouse version and K they were taken at, the verdict, and the date. Each
one stands on its own, so they can be read in any order and none of them
needs a second document to make sense.

Timings need a quiet machine. `DEVELOPING.md` says what that means and what
to record alongside a number.

## The live instrument

`clickdoom bench canonical`, or `make bench-canonical-throughput`, measures
real-ROM throughput on a boot window and a store-heavy gameplay window,
fold-alone and end to end, reported separately. A throughput claim should
come from it. It is an instrument rather than an experiment, so it has no
record here; `rom/bench/canonical_throughput/README.md` describes what it
measures and how it reaches the gameplay window without a multi-hour run.

## Throughput and the batch loop

| Record | Question it settles | What came out |
|---|---|---|
| [`arrayfold-baseline`](experiments/arrayfold-baseline.md) | Can `arrayFold` carry a CPU step, and what is the lever? | Yes. Pre-decoding is worth 7.4x. The price follows the step expression rather than the data it touches, and `short_circuit_function_evaluation = 'disable'` takes 11.7% off the production fold. |
| [`batch-attribution`](experiments/batch-attribution.md) | Where does one end-to-end batch's time go, and would a larger K amortise the setup? | A batch is 99.86% fold. Raising K is rejected. |
| [`write-log-growth`](experiments/write-log-growth.md) | Does per-instruction cost grow within a batch? | Yes, linearly in write-log length, at 3.41 ns per element per step, of which 79% is accumulator copy. Removing the scan is worth about 2% of a batch. |
| [`write-log-high-water-mark`](experiments/write-log-high-water-mark.md) | Where does the write-log flush stop being cheap? | The default of 20,000, at the bottom of the measured curve. Boot runs six instructions below it. |
| [`batch-overhead-split`](experiments/batch-overhead-split.md) | Does end-to-end overhead come from state reload or the write-log flush? | No result recorded. |
| [`halt-semantics-cost`](experiments/halt-semantics-cost.md) | What do the fold's halt semantics cost? | No result recorded. |

## Expression evaluation

| Record | Question it settles | What came out |
|---|---|---|
| [`compiled-node-cost`](experiments/compiled-node-cost.md) | What does one expression node cost in a fold step? | 4.4 ns compiled, 0.29 us interpreted. The recorded per-node price was the literals the nodes carry. |
| [`expression-jit`](experiments/expression-jit.md) | What does ClickHouse's expression JIT compile in the fold step, and what does that buy? | Small islands compile, worth 5.8% to 8.1%. Making more of the fold compilable is rejected. |
| [`subexpression-dedup`](experiments/subexpression-dedup.md) | Does `arrayFold` deduplicate repeated subexpressions, and at what node cost? | Dedup is structural rather than textual, and partial. Binding wins at the depth the fold emits. |
| [`block-dispatch`](experiments/block-dispatch.md) | What does an unselected branch cost inside `arrayFold`? | An unselected arm costs what a selected one costs. Static block translation rejected. |
| [`dict-lookup`](experiments/dict-lookup.md) | Would a dictionary beat the captured array for RAM reads? | No lever. The fold keeps the captured array. |

## Environment and the ROM

| Record | Question it settles | What came out |
|---|---|---|
| [`native-vs-docker`](experiments/native-vs-docker.md) | Is native ClickHouse faster than Docker, and how do two releases compare? | Docker is 2.07x faster than native on 26.3.25.2. Native rejected. |
| [`memcpy-memset-cost`](experiments/memcpy-memset-cost.md) | Are `memcpy` and `memset` byte-loop shims, and what do they cost? | They are newlib's and already word-wise, at 0.836 instructions per byte. Rejected. |
