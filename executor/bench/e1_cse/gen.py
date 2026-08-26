#!/usr/bin/env python3
"""Experiment E1: does ClickHouse deduplicate repeated subexpressions inside
an arrayFold lambda, and does that dedup survive cosmetic textual/AST noise?

Emits one SELECT for a named variant. See README.md for the question, the
protocol and the results.

Variants
--------
  floor              near-empty step (fold + tuple-copy overhead only)
  n1 n2 n5 n10 n20 n40
                     N byte-identical copies of a DEEP subexpression B,
                     the shape fold.py's `{B}` actually emits
  n40_ws             40 copies, AST-identical but textually different
                     (redundant parentheses + whitespace)
  n40_plus0_same     40 copies, all carrying the SAME two `+ 0` no-ops.
                     Byte-identical to each other; node count matches
                     n40_plus0_distinct exactly. This is the CONTROL that
                     prices the extra `+ 0` nodes on their own.
  n40_plus0_distinct 40 copies, each carrying a DIFFERENT pair of `+ 0`
                     no-ops -- same node count as n40_plus0_same, same
                     semantics, but no two copies are AST-identical.
  n40_bound          B evaluated ONCE and bound to a nested-lambda
                     parameter, then referenced 40 times. The alternative
                     optimisation, priced against n40.

Everything is deterministic: the decode fixture is seeded from `number`,
nothing calls now()/rand() (SPEC §8.1, scripts/check_purity.sh).  # purity-ok: this line documents the absence of now()/rand(), it doesn't call either

Usage: gen.py VARIANT [K]
"""
import itertools
import sys

DB = "e1_cse_bench"
RAM_BASE = 0x8000_0000
DECN = 524_288          # 2 MiB of text / 4, same as the Phase 0 fixture
NREGS = 31              # x1..x31, no x0 slot (schema.sql / fold.py)

# `+ 0` insertion sites. 6 inside IDX (which B embeds three times, as
# distinct copies a/b/c) + 7 at B's own level = 25 sites. All 25 add
# exactly two AST nodes (`plus` and the literal), so any two-site choice
# costs the same 4 nodes regardless of which two sites are chosen.
IDX_SLOTS = ["i_pc", "i_base", "i_sub", "i_sh", "i_clamp", "i_one"]
B_SLOTS = ["b_rs2a", "b_zero", "b_zlit", "b_rs2b", "b_read", "b_imm", "b_all"]


def _z(slots, sid):
    """A semantics-preserving `+ 0` if this site is enabled for this copy."""
    return " + 0" if sid in slots else ""


def idx(slots, tag):
    """fold.py's IDX: pc (byte address) -> clamped, 1-based decode index."""
    t = f"{tag}_"
    return (f"(least(bitShiftRight(toUInt32(toUInt64(acc.1"
            f"{_z(slots, t + 'i_pc')}) - {RAM_BASE}{_z(slots, t + 'i_base')})"
            f"{_z(slots, t + 'i_sub')}, 2{_z(slots, t + 'i_sh')}),"
            f" {DECN - 1}{_z(slots, t + 'i_clamp')}) + 1{_z(slots, t + 'i_one')})")


def build_b(slots=(), parens=0, pad=0):
    """The deep subexpression under test.

    Mirrors fold.py's `{B}` -- the ALU/branch second operand:

        toUInt32(if(rs2 = 0, 0, regs[rs2]) + imm)

    where rs2/imm are decode-array lookups and *each* lookup re-expands the
    full IDX subtree. Three IDX expansions + two tuple-element reads + one
    guarded register read: ~45 AST nodes, the depth fold.py really emits,
    as opposed to the shallow `BIG[bitAnd(acc,N)+1]` of the earlier probe.

    `parens` wraps the result in redundant parentheses and `pad` sprinkles
    whitespace -- both erased by the parser, so they change the TEXT without
    changing the AST.
    """
    slots = set(slots)
    sp = " " * pad
    r2a = f"DEC[{idx(slots, 'a')}].4{_z(slots, 'b_rs2a')}"
    r2b = f"DEC[{idx(slots, 'b')}].4{_z(slots, 'b_rs2b')}"
    imm = f"DEC[{idx(slots, 'c')}].5{_z(slots, 'b_imm')}"
    read = f"acc.2[{r2b}]{_z(slots, 'b_read')}"
    guarded = (f"if({r2a}{sp} = 0{_z(slots, 'b_zero')},{sp}"
               f" toUInt32(0{_z(slots, 'b_zlit')}),{sp} {read})"
               f"{_z(slots, 'b_all')}")
    b = f"toUInt32({guarded} + {imm})"
    return "(" * parens + b + ")" * parens


def copies(variant):
    """Returns the list of B texts for a variant."""
    if variant == "floor":
        return []
    if variant.startswith("n") and variant[1:].isdigit():
        n = int(variant[1:])
        return [build_b() for _ in range(n)]
    if variant == "n40_ws":
        # AST-identical, byte-different: k redundant parens + k spaces.
        return [build_b(parens=k % 7, pad=k % 5) for k in range(40)]

    # Two `+ 0` no-ops per copy, so every copy in both plus0 variants has
    # exactly the same node count (base + 4). The only difference between
    # the two variants is whether the 40 copies are identical to each other.
    sites = [f"{t}_{s}" for t in "abc" for s in IDX_SLOTS] + B_SLOTS
    pairs = list(itertools.combinations(sites, 2))
    if variant == "n40_plus0_same":
        return [build_b(slots=pairs[0]) for _ in range(40)]
    if variant == "n40_plus0_distinct":
        assert len(pairs) >= 40
        return [build_b(slots=pairs[k]) for k in range(40)]
    raise SystemExit(f"unknown variant: {variant}")


def build_query(variant, k, jit=0, salt=None):
    if variant == "n40_bound":
        # Bind B once to a nested-lambda parameter (a 1-element arrayMap is
        # the only let-binding ClickHouse's expression language offers), then
        # reference the *parameter* 40 times. Same value as n40.
        refs = " + ".join(["toUInt64(bv)"] * 40)
        bound = f"arrayMap(bv -> ({refs}), [{build_b()}])[1]"
        sink = f"toUInt32(bitAnd(toUInt64(acc.3) + {bound}, 4294967295))"
        return _wrap(_salted(sink, salt), k, jit)
    cs = copies(variant)
    if cs:
        # Sum the copies into a sink field. Addition (not xor) so no pair can
        # cancel; the sink feeds the fold's own output so nothing is dead.
        total = " + ".join(f"toUInt64({c})" for c in cs)
        sink = f"toUInt32(bitAnd(toUInt64(acc.3) + {total}, 4294967295))"
    else:
        sink = "toUInt32(acc.3 + 1)"
    return _wrap(_salted(sink, salt), k, jit)


def _salted(sink, salt):
    """Bake a per-batch literal into the step, the way fold.py bakes
    `icount_base` into TICKS_MS. Used to test whether a literal that changes
    every batch changes the compiled-expression cache key -- i.e. whether the
    JIT can ever warm up in production."""
    if salt is None:
        return sink
    return f"toUInt32(bitAnd(toUInt64({sink}) + {salt}, 4294967295))"


def _wrap(sink, k, jit=0):
    # pc advances by a constant 4, so the decode index walks the array
    # deterministically and the `least` clamp is never reached for k <= DECN.
    step = f"tuple(toUInt32(acc.1 + 4), acc.2, {sink})"
    return f"""WITH
  (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw))
     FROM (SELECT * FROM {DB}.decoded ORDER BY word_addr)) AS DEC,
  tuple(toUInt32({RAM_BASE}),
        arrayMap(j -> toUInt32(j * 2654435761), range(1, {NREGS + 1})),
        toUInt32(0)) AS INIT
SELECT r.1 AS pc, r.3 AS sink
FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), INIT) AS r)
SETTINGS max_threads = 1,
         compile_expressions = {jit},
         max_ast_elements = 4000000,
         max_expanded_ast_elements = 4000000"""


if __name__ == "__main__":
    v = sys.argv[1]
    kk = int(sys.argv[2]) if len(sys.argv) > 2 else 200_000
    jj = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    sl = int(sys.argv[4]) if len(sys.argv) > 4 else None
    # compile_expressions defaults to 1 in ClickHouse 26.3 and is a large,
    # order-dependent confound here: min_count_to_compile_expression = 3
    # means the *third* run of a variant can hit a warm LLVM cache and come
    # in 3x faster than the first two. Default this harness to 0 so the
    # measurement is of interpreted expression evaluation -- which is the
    # cost model fold.py's node budget is written against -- and pass 1
    # explicitly to measure the JIT's effect.
    print(build_query(v, kk, jj, sl))
