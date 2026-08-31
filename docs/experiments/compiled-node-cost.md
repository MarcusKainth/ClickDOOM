# What one expression node costs in a fold step

[`arrayfold-baseline.md`](arrayfold-baseline.md) priced a fold step at roughly
0.8 us per expression node and concluded that node count sets the price. This
prices a node with node count and literal count moved independently. A node is
4.4 ns compiled and 0.29 us interpreted. The rest of a recorded per-node figure
is the literals the nodes carry.

## Question

What does one function node in an `arrayFold` lambda cost, compiled,
interpreted, and unselected inside a guard? Does the cost follow node count,
and is emitting fewer nodes a lever on the production step?

## Method

The body under test is a chain of
`bitXor(bitAnd(plus(multiply(x, A), B), M), C)` links. One link is four
compilable UInt64 function nodes and four literals, and every literal is above
2^32 so it infers as UInt64 and no cast node appears. Three literal patterns
run at the same node counts:

| pattern | how the four literals are chosen | distinct literals in the chain |
|---|---|---:|
| `single` | one quadruple for every link | 4, whatever the length |
| `reused` | a quadruple cycled over four values | 16, whatever the length |
| `distinct` | its own quadruple per link | 4 per link, so one per node |

Reading `single` and `reused` against `distinct` at equal node count separates
the node from its literals. A `floor` arm whose step is
`plus(acc, 4294967311)` runs at every point and is subtracted, so a fit over
the points prices marginal nodes rather than the query's fixed cost.

The unselected-node price comes from the same chain wrapped in
`arrayMap(g -> chain, if(bitAnd(acc, 1) = 2, [acc], emptyArrayUInt64()))[1]`.
The guard reads the accumulator, so it cannot be folded away, and
`bitAnd(acc, 1)` is 0 or 1 and never 2, so the array is empty on every step and
the chain is never selected. Two families run: distinct literals held at 16
while node count moves, and distinct literals moving with node count, which is
the shape [`block-dispatch.md`](block-dispatch.md) measured.

The harness checks these rather than assuming them.

- Each point's fold result at `compile_expressions` 0 and 1 must agree with each
  other and must differ from the same fold seeded one higher. A chain that was
  folded away, or an arm that ran a different program, reads as a mismatch
  instead of as a fast arm.
- A gated point's result must equal the `floor` arm's result. That is what "the
  body did not run" means, measured rather than inferred from the timing.
- Durations come from `system.query_log.query_duration_ms` keyed by query id,
  never from wall clock around the client.
- One fresh container per point, so the compiled-expression cache is cold, and
  `min_count_to_compile_expression = 0` puts compilation on the first
  execution. `CompileFunction` and `CompiledFunctionExecute` are recorded per
  arm, so the regime an arm ran in is read out rather than assumed.

The production step's own node, action and island counts come from
`EXPLAIN json = 1, actions = 1` on two reconstructions of the step expression,
plus the fold's own ProfileEvents differenced between K = 2,000 and K = 1,000.
No `EXPLAIN` form descends into a lambda, which is why the counts need two
instruments.

## Conditions

| | |
|---|---|
| Date | 2026-08-31 |
| ClickHouse | 26.7.5.10, pinned image digest |
| Machine | Apple Silicon, 18 cores, Docker Desktop, one fresh container per point |
| Settings | `max_threads = 1`, `min_count_to_compile_expression = 0`; `compile_expressions` 0 and 1 as an arm |
| K | 100,000 for the node sweep, 200,000 for the gated sweep, 20,000 for the evaluated sweep |
| Repeats | 6 per point for the node sweep, 3 for the gated sweep |
| Load | 0.75 to 4.62 of 18 cores, one-minute average, recorded before and after every arm |

The box was not quiet during any arm. The owner's `grafana`, `loki`, `mimir`,
`tempo`, `postgres`, `redis`, `seaweedfs` and `alloy` containers, a macOS
Virtualization VM and the long-lived `clickdoom-ch` container ran throughout.
Every timed comparison here is paired inside one container in one window and
the load is recorded on both sides of each arm, so the ratios and the fitted
slopes stand. The absolute microseconds carry that condition and should not be
compared against a number taken on an idle box.

## Results

### A node's price is mostly its literals

Marginal microseconds per step at K = 100,000, median of 6 repeats, with the
`floor` arm at the same K subtracted. Four function nodes per link.

`compile_expressions = 1`:

| nodes | `single`, 4 literals | `reused`, 16 literals | `distinct`, one literal per node |
|---:|---:|---:|---:|
| 4 | | | 0.66 |
| 8 | | | 1.18 |
| 16 | 0.93 | 3.24 | 2.95 |
| 32 | | 3.09 | 5.36 |
| 64 | | 3.29 | 14.76 |
| 128 | | 3.43 | 35.07 |
| 256 | 1.41 | 3.88 | 64.52 |
| 512 | 3.19 | 5.25 | 144.42 |

`compile_expressions = 0`:

| nodes | `single` | `reused` | `distinct` |
|---:|---:|---:|---:|
| 4 | | | 1.02 |
| 8 | | | 2.15 |
| 16 | 2.98 | 5.23 | 4.97 |
| 32 | | 7.39 | 9.50 |
| 64 | | 12.50 | 25.06 |
| 128 | | 25.15 | 55.52 |
| 256 | 22.70 | 57.34 | 122.20 |
| 512 | 146.12 | 150.32 | 296.20 |

At 16 nodes `reused` and `distinct` are the same expression, and they measure
3.24 against 2.95 compiled and 5.23 against 4.97 interpreted. They ran in
separate containers, so that gap is this instrument's spread between
containers.

Least squares over the points, with 95% intervals from the residual standard
error:

| pattern | compiled ns per node | R2 | interpreted ns per node | R2 |
|---|---:|---:|---:|---:|
| `single`, 4 literals | 4.58 +/- 0.82 | 0.9998 | 297 +/- 339 | 0.9920 |
| `reused`, 16 literals | 4.19 +/- 0.99 | 0.9720 | 292 +/- 54 | 0.9825 |
| `distinct`, one per node | 282 +/- 14 | 0.9975 | 575 +/- 52 | 0.9920 |

A compiled node with no new literal costs 4.2 to 4.6 ns. The same node carrying
its own literal costs 282 ns, 64x more. Interpreted, the node is 0.29 us and the
literal adds another 0.28 us.

The `single` interpreted fit has three points and an interval wider than its
own slope, so it corroborates `reused` and carries nothing on its own.

The literal can also be read off the intercepts, which is an independent
reading because it holds node count fixed and moves only the literal count.
The compiled intercept is 0.843 +/- 0.249 us at 4 distinct literals and
2.992 +/- 0.239 us at 16, so twelve more literals cost 2.149 us per step, or
0.179 us each.

### An unselected node

Least squares over every repeat of the gated sweep, 95% intervals from the
residual standard error, `compile_expressions = 1`.

| distinct literals | K | node state | ns per node | R2 |
|---|---:|---|---:|---:|
| 16, held | 200,000 | unselected, empty-array gate | 12.7 +/- 1.2 | 0.976 |
| 16, held | 20,000 | evaluated | 225.1 +/- 9.6 | 0.997 |
| one per node | 200,000 | unselected, empty-array gate | 171.5 +/- 8.6 | 0.994 |
| one per node | 20,000 | evaluated | 501.1 +/- 16.0 | 0.998 |

An unselected node is 17.8x cheaper than an evaluated one. The 0.15 to 0.20 us
per unselected node in [`block-dispatch.md`](block-dispatch.md) reproduces at
0.1715 us on the family whose literals move with node count, and 0.159 us of
that 0.1715 is the literal.

Three instruments price a distinct captured literal: 0.159 us from the
gated sweep, 0.179 us from the compiled intercepts, and about 0.28 us from the
`distinct` slope where the node consuming it also runs. Call it 0.16 to 0.28 us
per literal per step, the low end when its consumer is skipped.
[`subexpression-dedup.md`](subexpression-dedup.md) reached the same effect from
the other side: AST-distinct copies cost about 2.1x AST-identical ones at equal
node count.

### What the production step holds

The planner splits the step across three lambda scopes, and the counts below
are their sum.

| | |
|---|---:|
| FUNCTION nodes per step | 319 |
| captured constant columns, the `(acc, i)` lambda | 62 |
| actions per step | 318 |
| JIT islands in the step expression | 65 |
| island executions per step | 41 |
| `result_name` bytes copied per step | 1,350,909 |

`ExpressionActions::executeAction` assigns `res_column.name` before it reaches
the `is_lazy_executed` branch, so every action pays that copy every step
whether or not it runs.

Per-step ProfileEvents, K = 2,000 minus K = 1,000 over the 1,000 verified steps
in between:

| | `FunctionExecute` | `CompiledFunctionExecute` |
|---|---:|---:|
| production defaults | 163.334 | 41.0 |
| `compile_expressions = 0` | 245.334 | 0 |

`CompiledFunctionExecute` is incremented alongside `FunctionExecute` rather than
instead of it, so the production step runs 41 compiled actions and 122
interpreted ones.

## Verdict

A node is not a unit of cost. What a node costs depends on whether it is
compiled, at 4.4 ns, or interpreted, at 0.29 us, and on how many distinct
literals it brings with it, at 0.16 to 0.28 us each per step. The per-node
figures in the earlier records were taken on chains that carry a new literal on
every node, so they price a node and its literal together.

Cutting node count without cutting literals or moving work into the compiled
fraction buys 4.4 ns per node.

On the production step that arithmetic is 41 compiled actions at 4.4 ns, under
0.2 us, against a boot step of 218.1 us. The 122 interpreted actions priced at
this chain's interpreted rate of 0.225 to 0.292 us come to 27.5 to 35.7 us, 13%
to 16% of the step. That rate is for a two-argument arithmetic action, and the
step's `arrayLastIndex` over the write log and its register-array rebuild cost
more than that, so treat the figure as a floor on what removing every
interpreted action would buy rather than a ceiling.

The step lambda captures 62 constant columns, and every distinct literal in the
step is one of them. The rest include the captured RAM and decode arrays, which
these rates do not price. The number this arithmetic wants is the count of
distinct scalar literals in the emitted step, and this experiment did not take
it.

## Limits

The chain under test is two-argument UInt64 arithmetic. It says nothing about
what an `arrayElement` on a 24 MiB captured array, an `arrayLastIndex` over the
write log or a tuple rebuild costs, and those are what the production step
spends its time in. Every application of these rates to the production step is
an extrapolation across that gap.

The 218.1 us per step is one figure from one chained series: batches 5 to 14 of
a boot run at K = 60,000 and HWM = 20,000, 4,584.5 instructions/sec end to end.
Batches 1 to 3 of the same series run at 3,807.5, because they are the batches
that are both uncompiled and holding a write log at the high-water mark.

The gated sweep runs at K = 200,000 and the evaluated sweep at K = 20,000. The
two are compared as slopes per step, and their durations are not comparable.

Nothing here measures whether wrapping a region of the real step in an
empty-array gate is a net win. The gate has a wrapper cost of its own that this
sweep subtracts out through the `floor` arm.
