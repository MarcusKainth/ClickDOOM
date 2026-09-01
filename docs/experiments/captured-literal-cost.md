# What a distinct captured literal costs per step

The fold's step expression captures constants, and `arrayFold` rebuilds the
capture set on every step. This prices one captured constant, separates that
price from the node count and expression width that arrive with it, and asks
what removing the fold's constants is worth.

[`compiled-node-cost.md`](compiled-node-cost.md) puts most of a node's price
in the literals it carries and leaves the literal count in the production step
untaken. This takes it, on the production fold rather than on a synthetic
chain.

## Question

What does one distinct captured literal cost per step in the production fold?
Is the cost a slope per constant or a fixed charge per capture set? Does
removing the fold's constants reach the throughput the unattributed per-step
time allows?

## Method

Arms add or remove constants in the generated step expression while the
executed instruction stream is held identical. Each arm is emitted from
`executor/examples/step_variants.rs` as one runnable statement, and it runs in
its own container, because the compiled-expression cache is server-global and
`min_count_to_compile_expression` is a process-static counter in
`src/Interpreters/ExpressionJIT.cpp` that no `SYSTEM` statement resets.

Adding constants confounds three things at once. The arm that adds 60
constants also adds 60 `multiIf` arms and 61 function nodes, so its
per-constant figure charges the literals for all three. Four arms separate
them:

| arm | added nodes | added function nodes | added constants | added `multiIf` arms |
|---|---:|---:|---:|---:|
| `dup-constants` | 3 | 2 | 1 | 60 |
| `more-nodes` | 133 | 132 | 1 | 60 |
| `more-values` | 63 | 2 | 61 | 60 |
| `more-constants` | 121 | 61 | 60 | 60 |

Shape deltas are read from `EXPLAIN json=1 actions=1` over the flat step.
Every added branch is unreachable, because `hc` is `UInt8` in 0 to 8 and the
added arms compare against 33 or 222. The optimizer does not prune duplicate
`multiIf` conditions, which the node counts confirm.

A decision rule was written down before the block finished. If cost is per
action node, `dup-constants` to `more-nodes` costs +47.7 us per step. If cost
is per captured constant, that contrast costs about zero.

A separate sweep varies only how many constants are added, at 1, 15, 30, 60
and 90 through `CLICKDOOM_ADDED_CONSTANTS`, giving capture sets of 63 to 152.
The shape of cost against constant count is then read off directly instead of
being inferred from two points. A slope and a fixed per-capture-set charge
predict different shapes there, and nothing else in the arm changes.

Two further arms bound the instrument. `short-binding-param` renames a bound
parameter and is otherwise DAG-identical to baseline, so it must read zero.
`fewer-constants` removes 11 constants and is the only arm that goes
downward.

### Measurement discipline

- Fresh container per arm and per mode, so the compile cache is cold for each.
- Four warm-up batches before the eight timed ones. Compilation fires on the
  4th execution, and boot batches 1 to 3 are the write-log-saturated ones.
- `CompileFunction` recorded per batch. It is a cache-miss counter, so it has
  to be non-zero on a warm-up batch and zero on every timed batch. Both are
  checked in the data rather than assumed.
- `FORMAT Null` on the timed fold query.
- Retired count, stop reason, final `pc`, final `icount` and a
  `BCDIGEST`/`RAMDIGEST` signature asserted per arm, so a timing difference
  cannot be a work difference.
- The run is the independent unit. Intervals are Student t95 on per-run means
  paired by repeat, n = 5. Pairing at batch level would give intervals about
  3.6x tighter and is not used.
- Arm order rotates, so each arm runs at each position exactly once.

## Conditions

| | |
|---|---|
| Date | 2026-09-01 |
| ClickHouse | 26.7.5.10, pinned image `sha256:800e8286…` |
| ROM | `9a6a47d01119…`, the pinned hash |
| Window | boot, from reset |
| K | 60,000 (`CLICKDOOM_RUN_K`) |
| HWM | 20,000, `WRITE_LOG_HIGH_WATER_MARK_DEFAULT` |
| Batches | 4 warm-up, 8 timed, 480,000 retired per timed mode |
| Settings | `short_circuit_function_evaluation` disabled |
| Repeats | 5 for the decomposition and the first block, 3 for the sweep |
| Baseline | 184.929 us per instruction end to end, 5,407 instructions/sec |

The box is not quiet, and this measurement does not pretend otherwise. A
grafana, loki, mimir, tempo, postgres, redis, seaweedfs and alloy stack, a
long-lived ClickHouse server and a macOS Virtualization VM run throughout,
outside this protocol. Every arm is paired inside one window and the load
average is sampled at each run boundary. The 1-minute average runs 1.15 to
2.75 across the decomposition block, 1.36 to 2.53 across the sweep, and 1.51
to 3.58 across the first block. One run in the first block is contaminated
and is called out below.

Both timed modes are reported. `fold` times one select-only statement;
`e2e` adds every commit flush. They agree on every conclusion here, and `e2e`
is the quieter of the two, so `e2e` carries the headline figures.

## Results

### The confound

Paired against baseline, n = 5, Student t95, `e2e` then `fold`.

| arm | e2e us/step | fold us/step |
|---|---|---|
| `dup-constants` | +4.027 ± 1.257 | +4.052 ± 3.200 |
| `more-nodes` | +5.566 ± 1.789 | +6.500 ± 4.105 |
| `more-constants` | +23.095 ± 1.532 | +24.622 ± 5.199 |
| `more-values` | +42.646 ± 2.465 | +41.730 ± 3.118 |

Every one is resolved. The discriminating contrast is `dup-constants` to
`more-nodes`, which adds 130 function nodes and no constant. The node
hypothesis predicts +47.7 us and the measurement reads +1.539 ± 1.692 us
end to end. The node hypothesis is wrong by a factor of about 30.

The second discriminator agrees. The node hypothesis predicts +44.4 us for
baseline to `more-constants` and the measurement reads +23.095.

### The three terms

Dividing each contrast by what it varies gives three separable rates.

| term | contrast | e2e | fold |
|---|---|---:|---:|
| per action node | `dup-constants` to `more-nodes`, 130 nodes | 0.0118 us | 0.0188 us |
| per `multiIf` arm | baseline to `dup-constants`, 60 arms | 0.0671 us | 0.0675 us |
| per captured `UInt32` constant | `dup-constants` to `more-values`, 60 constants | 0.6436 us | 0.6280 us |

An action node costs 11.8 to 18.8 ns, so the whole 384-node baseline DAG is
about 4.5 us of a 185 us step. That is a direct in-fold measurement of the
node term, and it closes out node count as a candidate.

The `multiIf` width term is separable here for the first time.
`dup-constants` adds 60 arms while adding one constant and two nodes, and
costs +4.027 us per step. No earlier arm holds it fixed.

Subtracting the width and node terms from the baseline to `more-constants`
contrast leaves the narrow constant on its own:

```
e2e : (23.095 - 4.027 - 61 x 0.0118) / 60 = 0.3058 us per constant
fold: (24.622 - 4.052 - 61 x 0.0188) / 60 = 0.3237 us per constant
```

The undecomposed figure is 0.3849 us end to end, so about 20% of it is the
`multiIf` width the arm introduced alongside its literals.

A captured constant costs 25x to 55x what an action node costs. The literal
term survives the confound.

### The cost is a slope

The sweep, 3 repeats per point, means over the timed batches.

| added constants | capture set | fold us/step | e2e us/step |
|---:|---:|---:|---:|
| 1 | 63 | 188.449 | 186.159 |
| 15 | 77 | 191.413 | 191.965 |
| 30 | 92 | 196.606 | 196.109 |
| 60 | 122 | 206.929 | 209.170 |
| 90 | 152 | 226.158 | 221.111 |

```
e2e  OLS slope +0.3932 us per constant   R^2 = 0.9977   span 35.0 us
fold OLS slope +0.4187 us per constant   R^2 = 0.9666   span 37.7 us
```

Cost rises monotonically across 89 added constants. A fixed charge per
capture set is flat there, so it is ruled out. The 0.388 us first reported on
this fold is a slope.

The sweep adds one `multiIf` arm per constant, so its slope carries the width
term too. Subtracting the measured 0.0671 us per arm from the `e2e` slope
gives 0.315 us, against the 0.306 us measured independently in the
decomposition. Two blocks with different arms agree to 3%.

The sweep's `c60` point emits a query of 61,685 bytes, byte-identical in
length to the decomposition block's `more-constants`, so the two blocks run
the same SQL through different binaries and batch counts.

### The instrument reads zero when it should

`short-binding-param` is DAG-identical to baseline and reads +1.858 ± 5.242
us per step in fold mode, or +1.00% with the interval spanning zero. End to
end it reads +1.208 ± 9.165. The point estimate is 8% of what
`more-constants` shows, and the interval half-width is a quarter of
`more-constants`' effect.

The whole positive point estimate is one run. Dropping repeat 2 gives
+0.330 ± 4.564 us per step. That run is the one the load log puts at the
block's peak, with 5- and 15-minute averages of 3.49 and 3.01, the highest of
the 40 samples in that block. The control passes either way. It also shows
that the background stack is visible in the data at about the size of the
effects the downward arm was meant to resolve.

This fixes the instrument's floor. In fold mode the interval half-width is
5.2 us per step, or 2.8%, and the minimum difference detectable at 80% power
is 8.1 us, or 4.4%. End to end it is 14.2 us, or 7.6%.

### The downward arm does not resolve

`fewer-constants` removes 11 constants and reads -2.044 ± 4.193 us per step,
or -1.10% with the interval twice the effect. The sign cannot be read off it.
Resolving -1.0% at 80% power needs 26 repeats. The arm is confounded anyway,
because it also removes function nodes and island executions.

No measurement in this record removes a constant and observes a speedup.

### The capture set the fold holds

`EXPLAIN json=1 actions=1` over the baseline flat step finds 64 distinct
`COLUMN` nodes: 34 `UInt8`, 15 `UInt32`, 5 `UInt16`, 4 `_CAST`, 3 captured
arrays, 2 `Bool` and 1 `IN` set.

Classifying each by every function that consumes it, 20 are only
instruction-id dispatch discriminators of the form `equals(d.id, N)`. Six are
the values 0 and 1 written at different types, spelled `0_Bool`, `0_UInt8`,
`_CAST(0_UInt32)` and five spellings of 1. The capture set pays per value and
type pair, so those two numbers occupy eight entries.

### What removing it is worth

Against the 184.929 us per step baseline, charging every captured constant at
the narrow rate gives 64 x 0.306 = 19.6 us. Charging the 24 wide ones at the
`more-values` rate instead gives 36 x 0.306 + 24 x 0.644 = 26.5 us, with the
3 arrays and the `IN` set left at zero because they are not measured.

| what is removed | us/step saved | us/step left | instructions/sec |
|---|---:|---:|---:|
| every captured constant | 19.6 to 26.5 | 158.5 to 165.3 | 6,048 to 6,311 |
| the reachable subset | 9.3 | 175.6 | 5,694 |

The reachable subset is the 20 id-dispatch constants, each of which also
removes a `multiIf` arm and so is charged 0.306 + 0.067, plus the 6
type-unification duplicates at 0.306. That is +5.3% on 5,407
instructions/sec.

[`short-circuit-and-gameplay.md`](short-circuit-and-gameplay.md) puts the
ceiling on all node-level work at 6,200 to 6,590 instructions/sec. Removing
every captured constant lands at 6,048 to 6,311, which is at or under that
ceiling.

Reaching 7,330 to 7,880 instructions/sec needs 126.9 to 136.4 us per step,
which is 48.5 to 58.0 us off the baseline. The entire capture set is worth
19.6 to 26.5 us.

### The mechanism

`arrayFold` in `src/Functions/array/arrayFold.cpp` calls `filter` on the
lambda column and then `cloneResized` on the result, once per slice, which
for this one-row fold is once per step. `ColumnFunction::filter` and
`ColumnFunction::cloneResized` in `src/Columns/ColumnFunction.cpp` each start
with `ColumnsWithTypeAndName capture = captured_columns;` and then walk every
entry. That is two full passes over the capture set per step, each entry
carrying a name copy, shared-pointer traffic and a column allocation.

This predicts cost linear in the number of captures and independent of action
node count, which is the shape both blocks measure.

### Work identity

Across the 60 timed runs in the three blocks, every timed batch retires
exactly 60,000 with `stop = full_k`, and the high-water mark never binds on a
timed batch. `CompileFunction` is non-zero on warm-up batches and 0 on every
timed batch of every arm. `CompiledFunctionExecute` is exactly 2,340,000 on
every timed batch and `FunctionExecute` sits in a band of 8.034 to 8.040
million that is common to all arms, so the added nodes are fused by the JIT
and cost nothing the action counters can see. The correctness gate produces
one identical `BCDIGEST`/`RAMDIGEST` signature for every arm built, including
the width arm.

The +12.5% that `more-constants` costs end to end is not more interpreted
work. It is paid somewhere the action counters do not reach, which constrains
any further theory of the mechanism.

## Verdict

The captured-literal term is real. A distinct captured constant costs 0.306
us per step at `UInt8` in an `equals` right-hand side and 0.644 us at
`UInt32` in a `multiIf` branch value, both n = 5, against 0.0118 us for an
action node. The cost is a slope in the number of captures, established over
capture sets of 63 to 152 with R^2 = 0.9977 end to end. A fixed charge per
capture set is ruled out by the same sweep.

Two earlier figures should be read down. The 0.388 us first reported on this
fold and the 0.393 us sweep slope both bundle the 0.067 us `multiIf` width
term, because both arms add one arm per constant. In the production fold,
removing a constant does not generally remove an arm.

The term is not a reachable throughput lever. Baseline's 64
constants are 10.6% to 14.3% of the step. Removing all of them, which is not
an available move because the memory map bounds, the ROM text range and the
decode width have to be captured somewhere, reaches 6,048 to 6,311
instructions/sec. That is inside the ceiling already measured, and the
7,330 to 7,880 the unattributed time seemed to allow needs about twice what
the whole capture set is worth.

What is reachable is about +5.3%. Six type-unification duplicates are
mechanical and worth 1.8 us per step. The 20 id-dispatch constants are the
rest, and they are harder than the count suggests. Replacing the dispatch
with a per-opcode decode column trades 20 `equals` constants for about 20 new
tuple-index constants and nets nothing. Moving the memory-map constants into
the accumulator hits the same substitution. Only a packed lookup indexed by
`id` removes them.

Roughly 20 to 60 us of the 184.929 us boot step stays unattributed even in
the impossible case, and none of node count, region gating, name copying or
captured literals accounts for it. That remainder is larger than any term
this record prices.

## What went wrong along the way

The first report on this fold was written one run into a chain of twenty, and
the chain kept running after it was filed. Its headline 0.388 us turns out to
be repeat 1 of 5, at the high end of that block, against that block's mean of
0.368 us in the same mode. A report written mid-chain has to say which runs it
covers.

The same over-run had already answered the question the report left open. The
linearity sweep that decides slope against fixed charge was launched by the
same script and finished on disk hours before anyone looked for it. Re-running
it would have spent a timing window on a completed measurement.

Attribution nearly went wrong in a way worth recording. The first block's
records carry a null `note` field, so nothing inside a record names its arm,
and matching by timestamp looked like the only route. Three other channels
were already there. The block writes a ledger naming arm, repeat, position,
exit code and load average per run, the per-arm logs are named by arm, and the
first batch's `query_len` partitions the arms with no overlap at 60,236,
60,215, 59,793 and 61,685. Time matching alone would also have failed on one
sweep record, stamped 09:22:50 against a ledger time of 09:22:51, because the
driver writes the record just before the shell reads the clock. Five channels
agree on all 20 records.

A role table for the capture set was built on the assumption that a node's
`Arguments` indices in the `EXPLAIN` JSON index into the `Actions` array. A
spot check on the first node made that look right. It is wrong, and 418 of
741 argument names fail to appear in their parent's result name. The table
was discarded and rebuilt by parsing argument names out of each node's result
name, which validates cleanly. Anyone parsing these plans should check the
index space rather than the first node that happens to line up.

## Limits

Every arm here adds constants. No measurement removes one and observes a
speedup, because the only downward arm does not resolve at n = 5 and needs 26
repeats. The whole prize estimate extrapolates an upward slope back through a
region the slope was never measured in, and it extrapolates below the
smallest capture set measured, which is 63.

The 2.1x gap between the narrow and wide constant rates is not attributed.
`more-values` differs from `more-constants` in both the constant's byte width
and its argument position. `more-constants-wide` was built to separate them
by putting `UInt32` constants in the same `equals` right-hand side, and it
reads like the wide arm, which points at width. That block was cut at 4 of its
12 runs to release the timing lock, so every arm in it has 1 repeat and the
wide rate is reported as indicative at n = 1. The gap is about 17 us against a
block-to-block spread of about 5 us, so the direction is probably real. The
source does not predict it. `ColumnConst::cloneResized` and
`ColumnConst::filter` both share the inner data column, so a wide constant
should copy no more than a narrow one. The gap decides whether the capture set
costs 20 us or 27 us, because 15 `UInt32` and 4 `_CAST` entries of the 64 sit
on the wide side, and the 27 us figure charges the 5 `UInt16` entries with
them.

The 3 captured arrays and the `IN` set are not priced. The source copies them
on the same two passes as the scalars, and an array entry is not obviously
the same cost as a scalar entry. They are charged zero above, so the upper
bound on removing the capture set is understated by an unknown amount.

The two timed modes disagree about curvature in the sweep and only `fold`
shows any. `e2e` is straight, with a quadratic term rejected at F(1,2) = 0.80
against a 5% threshold of 18.5. `fold` gives F(1,2) = 24.29 with positive
convexity and segment slopes climbing 0.212, 0.346, 0.344 and 0.641. The test
has 2 degrees of freedom and the `fold` sweep ran 4 timed batches against the
other block's 8. If that convexity is real, the slope near 64 constants is
below the fitted 0.42 and the prize is smaller than the linear fit says.

The constant count is 64 by `EXPLAIN`. An earlier analysis script used 62.
The deltas between arms are unaffected, so every per-constant rate stands;
only the extrapolation over the whole capture set moves, by about 3%.

Only the boot window is measured. Gameplay has a different instruction mix
and a different store density, and nothing here says the per-constant rate
carries across.
