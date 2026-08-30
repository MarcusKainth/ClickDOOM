# B2 results: what an unselected branch costs inside arrayFold

Harness and how to rerun: [README.md](README.md).

## Results, 2026-08-29, best of three

| variant | seconds | marginal per step | per unselected node |
|---|---|---|---|
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

Fault probes (the guarded body divides by `toUInt64(cond)`, which is 0): none of `if`, `multiIf`, `arrayMap`, `arrayFold` raised. With `short_circuit_function_evaluation='disable'` the `if` form does raise. So on 26.7 short-circuit evaluation masks faults inside a fold lambda, but every node of the unselected branch is still called and still costs the full per-node price.

## Reading

- `if` and `multiIf` give nothing. An unselected arm costs the same as an evaluated one, as on 26.3.
- A lambda run over an empty array is the only cheap shape, at 0.15 to 0.20 us per node. That is 5 to 6 times cheaper than evaluation, not free, and it does not improve with grouping: 50 blocks cost 50 times one block.
- The real fold on this pin costs about 267 us per step (60,000 steps in 16 s). Every translated block adds about 16 us per step whether or not it runs. Ten hot blocks add 60% to every step and remove at most the steps they cover. The model gives 1.3x to 1.5x for 10 to 50 blocks, and a loss beyond about 80 blocks.

## Decision

REJECT static block translation as a route to six-figure instructions per second. The dispatch cost is per node per step for every translated block, selected or not. The ceiling is about 1.5x with a handful of hot blocks, at the price of a translator, a `blocks` table, SPEC changes to batch termination and checkpoint boundaries, and a PURITY ruling. Stage 2 (translated-block cost, SQL size limits) is not run.
