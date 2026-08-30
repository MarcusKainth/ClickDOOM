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
