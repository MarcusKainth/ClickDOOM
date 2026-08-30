# Write-log seeding results: does per-instruction cost grow within a batch?

Harness and how to rerun: [README.md](README.md).

## What it does that #180 could not

Varies write-log length **independently of K**. Nothing in normal operation can
do that — the log only grows by retiring stores — so `fold.select_only()` gained
a `wl0=` parameter that seeds the initial accumulator's write-log with `L0`
inert entries. Then K, the starting state, the query text and the compiled
lambda are all held constant and only `L0` moves. The slope
`d(fold_ms)/d(L0)` is read off directly instead of being fitted out of a
three-parameter model.

`slope / K` is the per-element, per-step cost. A linear scan predicts it is
**constant across K**, and that constancy — checked at several K — is the
mix-free answer to the headline question. It depends on no fit at all.

## Why not the obvious experiment

"Run single batches of length N from one checkpoint and fit `a + bN + cN²`"
compares *different instruction ranges* of a non-homogeneous window, so a fitted
curvature is partly instruction mix. `executor/bench/commit_mutation/ksweep.sh`'s
header makes this argument at length and is why that harness holds the window
fixed instead. Seeding sidesteps it: `L0` changes the write-log without changing
which instructions run.

## Why the seed is inert

`fold._addr_and_align`'s `wa_safe` clamps every word index with
`least(…, ram_words - 1)` — for *every* address, valid or not. So a real load's
`WA` is always `<= ram_words - 1`, and a seeded address of `UInt32::MAX` can
never satisfy `z = WA`. That is a property of the clamp, not of the address set
this ROM happens to touch, so it needs no empirical address analysis. The scan
is still walked in full: `arrayLastIndex` seeks the *last* match and cannot
short-circuit on a miss.

The structural argument is necessary but not sufficient, so it is also
executed — `executor/tests/test_fold.py::test_wl0_seed_is_inert_when_executed`
and `::test_wl0_seed_never_matches_a_real_load`. That has already earned its
keep; see below.

## The design that did not survive contact

The first version had five shapes, seeding acc.3's lanes *selectively*: lanes
`.1` and `.2` are both `Array(UInt32)` and only `.1` is scanned, so
`g_scan = g_V1 - g_V2` looked like a free way to split scan cost from copy cost.

**It is wrong.** `acc.3`'s lanes are parallel arrays and `LW` subscripts the
value lane with an index found in the address lane:

```sql
acc.3.2[arrayLastIndex(z -> z = WA, acc.3.1)]
```

Unequal lane lengths desynchronise them. With the addr lane seeded 8 deep and
the val lane empty, a real store lands at addr index 9 and val index 1, and a
load that should forward `0x1234` returns `0`. So an unequal seed is not inert
and would have timed a *different program* — silently, with a plausible-looking
number. The executed inertness test caught it on its first run.

Only equal-length seeds are valid, which is why `seed.py` has exactly two
shapes. `test_unequal_lane_seed_breaks_forwarding` pins the finding so the
elegant version cannot come back.

**Consequence:** the scan/copy split cannot come from subtraction inside the
fold. It comes from `micro.py` instead — standalone `arrayLastIndex` and
`arrayPushBack` throughput — compared against the measured slope. Two
instruments sharing no assumption is a real anchor; a subtraction between two
arms of one instrument was never going to be (#197).

## Inference, then observation

Comparing the sweep's slope against the primitive rates can only establish that
the fold is *cheaper* than a naive reading of its text implies. It cannot say
which of the two candidate reasons is responsible, because both produce the
same shortfall. So `micro.py` also carries two probes that test each hypothesis
**directly**, outside the fold:

- **H1 — does `arrayPushBack` copy the accumulator?**
  `arrayFold((acc, i) -> arrayPushBack(acc, i), range(N), [])` is O(N²) if the
  accumulator is copied each step and O(N) if it is mutated in place. The
  growth exponent over a sweep of N answers it, and the answer is 1 or 2 rather
  than a ratio needing interpretation.
- **H2 — does ClickHouse collapse the repeated scan?** Two folds over identical
  data, one evaluating `arrayLastIndex` once per step and one evaluating the
  byte-identical call six times. Equal cost means common subexpression
  elimination is firing — which is exactly the condition `LW`'s triple textual
  expansion creates in the real step expression, and what #191 separately found
  for the double scan.

The distinction matters for what may be claimed. Without these probes, the
mechanism is an inference from three measurements that happen to be consistent
with it; with them it is observed. And the inference would have been **half
wrong**: the pre-probe best fit was "one scan, no copy", but H1 shows the
accumulator *is* copied — the copy is simply cheap (0.633 ns/element against the
scan's 2.671), not absent.

Two things this got wrong before the probes ran, recorded because both are easy
to repeat:

- **The copy rate was overstated ~30×** by a microbenchmark that forced
  materialisation with `cityHash64` over the pushed arrays. The hash is itself
  O(length) and dominated the measurement. Scaffolding has to be cheaper than
  the thing it scaffolds.
- **The growth exponent was read too early.** At N ≤ 80,000 it reads ~1.43,
  which is ambiguous between O(N) and O(N²); it only reaches 1.97 by
  N = 320,000. An exponent has to be read where the asymptote is, not where the
  sweep happens to stop.

## What it found

Recorded here only as a pointer — the numbers and their provenance live on #257.

Per-instruction cost **does** grow within a batch, linearly in write-log length,
at 3.41 ns per element per step (95% CI [3.27, 3.61]). `slope/K` moves 7.0%
across a 4× range of K, which is the mix-free statement of that and rests on no
model. It reconciles to 0.74× with #180's independent fit.

The mechanism is fully accounted for: the load-forwarding scan is **81%** of the
term and the accumulator copy **19%** (2.671 + 0.633 = 3.304 predicted against
3.411 measured).

The actionable consequence is a **negative** one, which is why this harness
exists rather than an optimisation: ClickHouse already collapses all six textual
`arrayLastIndex` calls into one, so #180 §6's "bind the scan once" has no
headroom. And since the write-log term is 10.3% of a batch (#180) and the scan
is 81% of that, **the entire linear-scan mechanism is ≈8% of a batch** — which
bounds every structure-replacement idea competing for it, including #170's
already-rejected `Map`.
