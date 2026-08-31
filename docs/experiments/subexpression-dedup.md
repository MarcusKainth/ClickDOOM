# Subexpression dedup inside an arrayFold lambda

The fold's step expression is built by string substitution, so the same
subtrees appear many times. This measures what the repetition costs and
whether ClickHouse removes it.

## Question

Does ClickHouse deduplicate repeated subexpressions inside an `arrayFold`
lambda? If it does, does the dedup need the copies to be byte-identical, in
which case the generator would have to normalise the text it emits, or is
being AST-identical enough?

## Method

One generated `SELECT` per variant. The subexpression under test mirrors the
fold's operand read:

    toUInt32(if(rs2 = 0, 0, regs[rs2]) + imm)

with `rs2` and `imm` as decode-array lookups, each re-expanding the full
clamped index subtree, so one copy is about 100 parsed AST nodes.

| variant | what it is |
|---|---|
| `floor` | a near-empty step, the sanity baseline |
| `n1` to `n80` | that many byte-identical copies |
| `n40_ws` | 40 copies, AST-identical, text different through extra parentheses and whitespace |
| `n40_plus0_same` | 40 copies each carrying the same two `+ 0` no-ops, the node-count control |
| `n40_plus0_distinct` | 40 copies each carrying a different pair of `+ 0` no-ops, so equal node count and equal semantics but no two copies AST-identical |
| `n40_bound` | the subexpression evaluated once, bound to a nested-lambda parameter, referenced 40 times |

Every variant returns the same sink value, 1975151232, and the sink is in
the output, so a variant that silently lost a copy is visible.
`max_ast_elements` and `max_expanded_ast_elements` are raised to 4,000,000
to match what the real step expression needs. The decode fixture is seeded
entirely from `number`. Timing is client-side wall clock.

An earlier probe had measured a shallow subexpression at 10 copies and found
only partial dedup, 1.7x rather than 10x. This measures at the depth and
count the generator actually emits.

## Conditions

| | |
|---|---|
| Date | 2026-08-26 |
| ClickHouse | 26.3 |
| Settings | `max_threads = 1` |
| K | 50,000 |
| Repeats | 5, medians reported |

`marg` below is the median minus the `floor` median. `xB` is `marg`
relative to `n1`'s.

## Results

### Interpreted, `compile_expressions = 0`, floor 0.605 s

| variant | nodes | median | min | max | marg | xB |
|---|---:|---:|---:|---:|---:|---:|
| floor | 117 | 0.605 | 0.570 | 0.630 | | |
| n1 | 219 | 1.516 | 1.431 | 1.586 | 0.911 | 1.00 |
| n2 | 319 | 1.569 | 1.480 | 1.627 | 0.964 | 1.06 |
| n5 | 619 | 1.817 | 1.714 | 1.877 | 1.212 | 1.33 |
| n10 | 1,119 | 2.332 | 2.207 | 2.481 | 1.726 | 1.90 |
| n20 | 2,119 | 3.795 | 3.493 | 3.882 | 3.189 | 3.50 |
| n40 | 4,119 | 9.310 | 8.965 | 9.907 | 8.704 | 9.56 |
| n60 | 6,119 | 16.628 | 16.519 | 17.120 | 16.023 | 17.60 |
| n80 | 8,119 | 27.477 | 27.023 | 28.505 | 26.872 | 29.51 |
| n40_ws | 4,119 | 9.582 | 8.883 | 9.797 | 8.977 | 9.86 |
| n40_plus0_same | 4,359 | 10.322 | 9.608 | 10.768 | 9.716 | 10.67 |
| n40_plus0_distinct | 4,359 | 21.455 | 19.791 | 22.324 | 20.849 | 22.90 |
| n40_bound | 427 | 2.802 | 2.655 | 2.895 | 2.197 | 2.41 |

### Compiled, `compile_expressions = 1`, floor 0.545 s

| variant | nodes | median | min | max | marg | xB |
|---|---:|---:|---:|---:|---:|---:|
| floor | 117 | 0.545 | 0.532 | 0.550 | | |
| n1 | 219 | 1.104 | 1.021 | 1.123 | 0.559 | 1.00 |
| n2 | 319 | 1.107 | 1.072 | 1.122 | 0.562 | 1.01 |
| n5 | 619 | 1.163 | 1.121 | 1.184 | 0.618 | 1.11 |
| n10 | 1,119 | 1.211 | 1.161 | 1.311 | 0.666 | 1.19 |
| n20 | 2,119 | 1.365 | 1.331 | 1.435 | 0.820 | 1.47 |
| n40 | 4,119 | 1.763 | 1.698 | 1.800 | 1.218 | 2.18 |
| n40_ws | 4,119 | 1.757 | 1.716 | 1.896 | 1.212 | 2.17 |
| n40_plus0_same | 4,359 | 2.032 | 1.975 | 2.043 | 1.487 | 2.66 |
| n40_plus0_distinct | 4,359 | 6.216 | 5.842 | 6.547 | 5.671 | 10.14 |
| n40_bound | 427 | 1.398 | 1.365 | 1.468 | 0.853 | 1.53 |

### A per-batch literal in the lambda body

`min_count_to_compile_expression = 3`, so the 4th run of a given expression
hits the compiled cache. The question is what the cache key contains. Run
times in seconds for `n40` at `compile_expressions = 1`.

| run | literal in the lambda is fixed | literal varies per run | value carried in the fold's `INIT` instead |
|---:|---:|---:|---:|
| 1 | 9.931 | 9.966 | 1.797 |
| 2 | 9.952 | 10.220 | 1.713 |
| 3 | 9.945 | 10.066 | 1.717 |
| 4 | 1.785 | 10.057 | 1.740 |
| 5 | 1.791 | 10.043 | 1.725 |
| 6 | 1.791 | 9.956 | 1.724 |

A literal that changes every batch changes the compiled-expression cache
key, so the compiler never engages: 10.0 s forever against 1.79 s once warm,
a factor of 5.6. Moving the varying value out of the lambda body and into
the fold's `INIT` tuple keeps the cache warm.

## Verdict

Dedup is real, partial, and structural rather than textual. `n40` and
`n40_ws` are indistinguishable at 9.310 s and 9.582 s despite differing
text, so redundant parentheses and whitespace cost nothing and the generator
has nothing to normalise. Of the 1,817 `least(bitShiftRight(` occurrences in
the real step expression, 1,781 are the byte-identical index subtree and the
other 36 are a genuinely different subtree, so the copies are already
byte-identical and already deduped.

The interpreted curve is sublinear up to about 10 copies and superlinear
past about 20. Ten copies cost 1.9x one copy, 40 cost 9.6x and 80 cost
29.5x. The real step expression is about 59,900 parsed AST nodes at
K = 50,000, far past `n80`'s 8,119, so it sits well inside the superlinear
regime.

AST-distinct copies cost about 2.1x AST-identical ones at equal node count,
20.849 s against 9.716 s marginal interpreted and 3.8x compiled. The
residual sharing is real, because each copy still contains sub-subtrees
identical to its neighbours', which is why the penalty is 2x rather than
40x.

Nested-lambda binding wins at this depth and count. `n40_bound` is 4.0x
cheaper interpreted and 1.4x cheaper compiled than `n40`, and cuts parsed
nodes from 4,119 to 427. That reverses the earlier shallow 10-copy probe,
where binding was slower than inlining.

The largest number here is not about dedup. A per-batch literal in the
lambda body defeats the compiled-expression cache permanently and costs
5.6x on this probe. That is the cost of a cache-key change on a synthetic
fold, and it does not carry to the real step expression:
[`expression-jit.md`](expression-jit.md) measures what compilation is worth
there.
