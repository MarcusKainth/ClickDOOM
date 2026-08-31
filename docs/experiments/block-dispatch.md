# What an unselected branch costs inside arrayFold

Static block translation compiles a hot basic block into its own SQL
expression and selects between blocks at run time. It only pays if a block
that is not selected costs close to nothing per step. This measures that.

## Question

What does an unselected branch cost inside an `arrayFold` lambda, and is any
dispatch shape cheap enough to make static block translation worth building?

## Method

The body under test is a 200-node chain of `bitXor(plus(x, c1), c2)` with
distinct constants. The guard is false on every step but reads the
accumulator, so it cannot be folded away. Each dispatch shape wraps that
chain and is compared against an unguarded chain and against a `floor`
variant whose step is `acc.1 + 1`.

Fault probes make the guarded body divide by the guard value, which is zero,
so a shape that really skips the body does not raise and one that evaluates
it does.

## Conditions

| | |
|---|---|
| Date | 2026-08-29 |
| ClickHouse | 26.7.5.10 |
| Machine | fresh container |
| Settings | `max_threads = 1`, `compile_expressions = 0` |
| K | 20,000 |
| Repeats | 3, best reported |

## Results

| variant | seconds | marginal per step | per unselected node |
|---|---:|---:|---:|
| floor (`acc.1 + 1`) | 0.088 | | |
| chain, unguarded, 200 nodes | 4.093 | 200 us | 1.00 us (evaluated) |
| `if(cond, chain, cheap)` | 4.093 | 200 us | 1.00 us |
| same, `short_circuit_function_evaluation='force_enable'` | 4.597 | 225 us | 1.13 us |
| `multiIf(cond, chain, cheap)` | 4.037 | 197 us | 0.99 us |
| same, `force_enable` | 4.673 | 229 us | 1.15 us |
| `arrayMap(x -> chain, if(cond, [acc.1], []))` | 0.705 | 31 us | 0.15 us |
| `arrayFold(step, range(toUInt64(cond)), acc.1)` | 0.824 | 37 us | 0.18 us |
| 10 guarded `arrayMap` blocks, 100 nodes each | 3.341 | 163 us | 0.16 us |
| 50 guarded `arrayMap` blocks, 100 nodes each | 20.347 | 1,013 us | 0.20 us |

None of `if`, `multiIf`, `arrayMap` or `arrayFold` raised on the fault
probe. With `short_circuit_function_evaluation='disable'` the `if` form does
raise. So short-circuit evaluation masks faults inside a fold lambda on this
version, while every node of the unselected branch is still called and still
costs the full per-node price.

`if` and `multiIf` give nothing: an unselected arm costs the same as an
evaluated one. A lambda run over an empty array is the only cheap shape, at
0.15 to 0.20 us per node. That is 5 to 6 times cheaper than evaluation
rather than free, and it does not improve with grouping, since 50 blocks
cost 50 times one block.

## Verdict

Static block translation is rejected as a route to six-figure instructions
per second. The dispatch cost is per node per step for every translated
block, selected or not.

The real fold on this version costs about 267 us per step, 60,000 steps in
16 s. Every translated block adds about 16 us per step whether or not it
runs. Ten hot blocks add 60% to every step and remove at most the steps they
cover. The model gives 1.3x to 1.5x for 10 to 50 blocks and a loss beyond
about 80 blocks.

The ceiling is about 1.5x with a handful of hot blocks, at the price of a
translator, a `blocks` table, a contract change to batch termination and
checkpoint boundaries, and a purity ruling. The second stage of the
experiment, which would have priced a translated block and the SQL size
limits, was not run.
