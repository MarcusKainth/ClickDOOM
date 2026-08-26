# E1 — common-subexpression elimination inside an `arrayFold` lambda

## The question

`executor/fold.py` builds its step expression by string substitution, so the
same subtrees appear many times: `{ID}` ~46x, `{A}` and `{B}` ~24x each, and
each *embeds the whole subtree beneath it* (`{B}` embeds a guarded register
read plus `{IMM}`, and expands `IDX` three times). The parsed step is
**~59,900 AST nodes** at K = 50,000.

> Does ClickHouse deduplicate repeated subexpressions inside an `arrayFold`
> lambda, and if so, does that dedup depend on the copies being **byte**-
> identical (in which case fold.py should normalise its emitted text) or
> merely **AST**-identical?

A prior probe measured a *shallow* subexpression (`BIG[bitAnd(acc,N)+1]`) at
10 copies and found partial dedup (1.7x, not 10x). E1 tests at the **depth
and count fold.py actually emits**.

## What the harness does

`gen.py` emits one `SELECT` per variant. The subexpression under test mirrors
fold.py's `{B}`:

    toUInt32(if(rs2 = 0, 0, regs[rs2]) + imm)

with `rs2`/`imm` as decode-array lookups, each re-expanding the full clamped
`IDX` subtree — ~100 parsed AST nodes per copy.

| variant | what it is |
|---|---|
| `floor` | near-empty step — the sanity floor (fold + tuple-copy overhead) |
| `n1 n2 n5 n10 n20 n40 n60 n80` | N **byte-identical** copies |
| `n40_ws` | 40 copies, **AST-identical, text-different** (redundant parens + whitespace) |
| `n40_plus0_same` | 40 copies each carrying the **same** two `+ 0` no-ops — the node-count control |
| `n40_plus0_distinct` | 40 copies each carrying a **different** pair of `+ 0` no-ops: identical node count and identical semantics, but no two copies are AST-identical |
| `n40_bound` | `B` evaluated once, bound to a nested-lambda parameter, referenced 40 times |

All variants return the same `sink` value (`1975151232`) where they should,
which is checked by the TSV's last column — a variant that silently lost a
copy would show a different sink.

Determinism: the decode fixture is seeded entirely from `number`; no `now()`,
no `rand()` (SPEC §8.1). Timing is client-side wall clock, which is
measurement and not computation (PURITY.md).

`max_ast_elements` / `max_expanded_ast_elements` are raised to 4,000,000 in
every generated query — the default 50,000 is not hit by these variants, but
it is hit by fold.py's real step, so the harness matches production.

## How to rerun

    cd executor/bench/e1_cse
    ./run.sh                       # compile_expressions = 0, K = 50000, 5 repeats
    E1_JIT=1 ./run.sh              # compile_expressions = 1 (the ClickHouse default)
    E1_K=200000 E1_REPEATS=3 ./run.sh

    # clean up (the container is shared)
    docker exec -i clickdoom-ch clickhouse-client --query 'DROP DATABASE e1_cse_bench'

The JIT sub-experiment (see below) is:

    python3 gen.py n40 50000 1 900001 > q.sql   # 4th arg = a baked-in literal
    # run q.sql 6x with the literal FIXED, then 6x with it VARYING

## Results (ClickHouse 26.3, `max_threads = 1`, K = 50,000, 5 repeats)

`marg` = median minus the `floor` median. `xB` = marg relative to `n1`'s marg.

### `compile_expressions = 0` (interpreted) — floor 0.605 s

| variant | nodes | median | min | max | marg | xB |
|---|---|---|---|---|---|---|
| floor | 117 | 0.605 | 0.570 | 0.630 | — | — |
| n1 | 219 | 1.516 | 1.431 | 1.586 | 0.911 | 1.00 |
| n2 | 319 | 1.569 | 1.480 | 1.627 | 0.964 | 1.06 |
| n5 | 619 | 1.817 | 1.714 | 1.877 | 1.212 | 1.33 |
| n10 | 1119 | 2.332 | 2.207 | 2.481 | 1.726 | 1.90 |
| n20 | 2119 | 3.795 | 3.493 | 3.882 | 3.189 | 3.50 |
| n40 | 4119 | 9.310 | 8.965 | 9.907 | 8.704 | 9.56 |
| n60 | 6119 | 16.628 | 16.519 | 17.120 | 16.023 | 17.60 |
| n80 | 8119 | 27.477 | 27.023 | 28.505 | 26.872 | 29.51 |
| n40_ws | 4119 | 9.582 | 8.883 | 9.797 | 8.977 | 9.86 |
| n40_plus0_same | 4359 | 10.322 | 9.608 | 10.768 | 9.716 | 10.67 |
| n40_plus0_distinct | 4359 | 21.455 | 19.791 | 22.324 | 20.849 | 22.90 |
| n40_bound | 427 | 2.802 | 2.655 | 2.895 | 2.197 | 2.41 |

### `compile_expressions = 1` (the default) — floor 0.545 s

| variant | nodes | median | min | max | marg | xB |
|---|---|---|---|---|---|---|
| floor | 117 | 0.545 | 0.532 | 0.550 | — | — |
| n1 | 219 | 1.104 | 1.021 | 1.123 | 0.559 | 1.00 |
| n2 | 319 | 1.107 | 1.072 | 1.122 | 0.562 | 1.01 |
| n5 | 619 | 1.163 | 1.121 | 1.184 | 0.618 | 1.11 |
| n10 | 1119 | 1.211 | 1.161 | 1.311 | 0.666 | 1.19 |
| n20 | 2119 | 1.365 | 1.331 | 1.435 | 0.820 | 1.47 |
| n40 | 4119 | 1.763 | 1.698 | 1.800 | 1.218 | 2.18 |
| n40_ws | 4119 | 1.757 | 1.716 | 1.896 | 1.212 | 2.17 |
| n40_plus0_same | 4359 | 2.032 | 1.975 | 2.043 | 1.487 | 2.66 |
| n40_plus0_distinct | 4359 | 6.216 | 5.842 | 6.547 | 5.671 | 10.14 |
| n40_bound | 427 | 1.398 | 1.365 | 1.468 | 0.853 | 1.53 |

### The JIT sub-experiment (`n40`, `compile_expressions = 1`)

`min_count_to_compile_expression = 3`, so the 4th run of a given expression
hits the compiled cache. The question is what the cache key contains.

| run | literal baked into the lambda is FIXED | literal VARIES per run | value carried in the fold's INIT instead |
|---|---|---|---|
| 1 | 9.931 | 9.966 | 1.797 |
| 2 | 9.952 | 10.220 | 1.713 |
| 3 | 9.945 | 10.066 | 1.717 |
| 4 | **1.785** | 10.057 | 1.740 |
| 5 | **1.791** | 10.043 | 1.725 |
| 6 | **1.791** | 9.956 | 1.724 |

A literal that changes every batch changes the compiled-expression cache key,
so the JIT never engages: 10.0 s forever, versus 1.79 s once warm — **5.6x**.
Moving the varying value out of the lambda body and into the fold's `INIT`
tuple keeps the cache warm.

## Findings

1. **Dedup is real but partial, and it is STRUCTURAL, not textual.**
   `n40` and `n40_ws` are indistinguishable (9.310 vs 9.582 s, inside
   run-to-run spread) despite differing text. Redundant parentheses and
   whitespace cost nothing. Byte-identity is not what matters — AST identity
   is.
2. **The interpreted curve is sublinear up to ~10 copies and superlinear
   beyond ~20.** 10 copies cost 1.9x one copy (reproducing the earlier
   shallow-probe number at realistic depth), but 40 cost 9.6x and 80 cost
   29.5x. fold.py's real step is ~59,900 nodes — 7x larger than `n80` — so it
   sits well inside the superlinear regime.
3. **AST-distinct copies cost ~2.1x AST-identical ones at equal node count**
   (`n40_plus0_distinct` 20.849 vs `n40_plus0_same` 9.716 marginal; 3.8x
   under JIT). The residual sharing is real: each copy still contains
   sub-subtrees identical to its neighbours', so the penalty is 2x, not 40x.
4. **fold.py has nothing to normalise.** Of the 1,817 `least(bitShiftRight(`
   occurrences in the real step, 1,781 are the byte-identical `IDX`; the
   other 36 are the genuinely different `WA` subtree. The macros are plain
   string substitutions of one string, so the copies are already
   byte-identical and already deduped.
5. **Nested-lambda binding wins decisively at this depth and count** —
   `n40_bound` is 4.0x cheaper interpreted and 1.4x cheaper JIT-warm than
   `n40`, and cuts parsed nodes 4,119 -> 427. This **reverses** the earlier
   shallow-10 probe, where binding was slower than inlining.
6. **The largest single number in this experiment is not about CSE at all:**
   a per-batch literal in the lambda body (fold.py's `icount_base`) defeats
   ClickHouse's compiled-expression cache permanently, costing 5.6x.
