#!/usr/bin/env python3
"""Experiment A1: why does ClickHouse's expression JIT only PARTIALLY compile
the arrayFold step expression, and can more of it be made compilable?

Emits one SELECT for a named variant. See README.md for the question, the
protocol and the results.

The probe shape is always the same sandwich inside an arrayFold lambda:

    sink = CHAIN(links/2, CONSTRUCT( CHAIN(links/2, toUInt64(acc.3)) ))

`CHAIN` is a nest of unambiguously-JIT-compilable UInt64 arithmetic
(4 function nodes per link, no casts, no type change), so nothing can
fragment it except the construct under test. `CONSTRUCT` is the single
expression whose compilability is being measured; it takes the lower chain's
value as its input and the upper chain continues from its result, so a
construct the JIT cannot compile necessarily CUTS the chain.

Ground truth is `system.query_log`'s ProfileEvents, NOT wall clock:

    CompileFunction                 = LLVM functions built = compiled ISLANDS
    CompileExpressionsMicroseconds  = time spent in LLVM
    CompileExpressionsBytes         = size of generated code (8 KiB granular)

Read against `chain_only` (exactly 1 island), the island COUNT answers the
decisive question directly:

    1 island  -> the construct is compilable and fuses into the chain
    2 islands -> the construct is NOT compilable, and it cuts ONLY ITSELF
                 out: the chain above and the chain below are each still
                 compiled, with the construct's result fed in as an input
    0 islands -> the construct POISONS the whole enclosing expression

Everything is deterministic: the fixture is seeded from `number`, nothing
calls now()/rand() (SPEC §8.1, scripts/check_purity.sh).  # purity-ok: this line documents the absence of now()/rand(), it doesn't call either

Usage: gen.py VARIANT [K] [JIT] [MIN_COUNT] [LINKS]
       gen.py --list
"""
import hashlib
import os
import sys

DB = "a1_jit_bench"
RAM_BASE = 0x8000_0000
DECN = 524_288
NREGS = 31

# Bumping A1_SALT_EPOCH gives every variant a fresh set of literals, which
# gives every island a fresh DAG hash. ClickHouse's compiled-expression cache
# is SERVER-GLOBAL and keyed by the island's DAG (not by its SQL text), so a
# rerun of an already-measured variant otherwise reports CompileFunction = 0:
# the counter is a cache MISS counter, not an island counter. Bump the epoch
# rather than `SYSTEM DROP COMPILED EXPRESSION CACHE`, which would perturb
# other agents' queries on the shared container.
EPOCH = int(os.environ.get("A1_SALT_EPOCH", "1"))
SALT = 0


def chain(x, links, start=0):
    """`links` nested compilable UInt64 ops. 4 function nodes per link.

    `start` offsets the per-link literals, so two chains built separately
    still get distinct literals -- needed by fragmented(), where otherwise
    every inter-cut island would be structurally IDENTICAL and the global
    compiled-expression cache would compile it once and report one island
    instead of N.

    Function-call form, not infix: a long left-nested chain of infix
    operators makes ClickHouse's recursive-descent parser backtrack
    super-linearly. Same AST, no operator ambiguity.
    """
    for j in range(links):
        k = start + j
        x = (f"bitXor(bitAnd(plus(multiply({x}, {2654435761 + k}),"
             f" {12345 + k + SALT}), 4294967295), {987654321 + k})")
    return x


def _multiif(x, arms):
    """N-arm multiIf. `x` appears 2N+1 times -- linear, not exponential."""
    a = "".join(f"equals(bitAnd({x}, 255), {k}), plus({x}, {k + 1}), "
                for k in range(arms))
    return f"multiIf({a}plus({x}, 99))"


# --- the constructs under test ------------------------------------------
# Each maps a UInt64 `x` (the lower chain's value) to an integer the upper
# chain continues from. IN-PATH: the chain flows THROUGH the construct, so a
# non-compilable construct must cut it. Value preservation is irrelevant and
# not attempted; what matters is that nothing here is a no-op the constant
# folder could erase, and that `x` is not duplicated exponentially.
CONSTRUCTS = {
    # -- controls ---------------------------------------------------------
    "chain_only":        lambda x: x,
    "plus_lit":          lambda x: f"plus({x}, 7)",

    # -- scalar candidates ------------------------------------------------
    "cast_u32":          lambda x: f"toUInt32(bitAnd({x}, 4294967295))",
    "cast_i32":          lambda x: f"toInt32(bitAnd({x}, 2147483647))",
    "cast_u64":          lambda x: f"toUInt64({x})",
    "cast_i64":          lambda x: f"toInt64(bitAnd({x}, 2147483647))",
    "cast_u8":           lambda x: f"toUInt8(bitAnd({x}, 255))",
    "intdiv":            lambda x: f"intDiv({x}, 3)",
    "modulo":            lambda x: f"modulo({x}, 4294967291)",
    # `divide` yields Float64, which the chain's bitAnd cannot consume, so
    # this one carries a toUInt64 back. Casts are themselves compilable
    # (see cast_u64), so a 1-island reading here is still `divide` fusing.
    "divide_float":      lambda x: f"toUInt64(divide({x}, 2))",
    "if_scalar":         lambda x: f"if(equals(bitAnd({x}, 1), 0), plus({x}, 3), plus({x}, 5))",
    "multiif_4":         lambda x: _multiif(x, 4),
    "multiif_12":        lambda x: _multiif(x, 12),
    "multiif_28":        lambda x: _multiif(x, 28),
    "least_greatest":    lambda x: f"least(greatest({x}, 3), 18446744073709551000)",
    "bitshift":          lambda x: f"bitShiftLeft(bitShiftRight({x}, 3), 3)",
    "compare":           lambda x: f"less({x}, 9223372036854775807)",
    "and_or_not":        lambda x: f"or(equals(bitAnd({x}, 1), 0), not(equals(bitAnd({x}, 2), 0)))",
    "abs_negate":        lambda x: f"abs(negate(toInt64(bitAnd({x}, 2147483647))))",
    "transform_fn":      lambda x: f"transform(toUInt8(bitAnd({x}, 3)), [0, 1, 2, 3], [7, 8, 9, 10], 11)",

    # -- array / tuple / map candidates -----------------------------------
    "tuple_ctor":        lambda x: f"tupleElement(tuple({x}, 1), 1)",
    "arrayelem_regs":    lambda x: f"arrayElement(acc.2, plus(bitAnd({x}, 30), 1))",
    "arrayelem_dec":     lambda x: f"tupleElement(arrayElement(DEC, plus(bitAnd({x}, {DECN - 1}), 1)), 5)",
    "arrayelem_literal": lambda x: f"arrayElement([11, 22, 33, 44], plus(bitAnd({x}, 3), 1))",
    "arraylastindex":    lambda x: f"arrayLastIndex(z -> equals(z, {x}), acc.2)",
    "arrayslice_len":    lambda x: f"length(arraySlice(acc.2, 1, plus(bitAnd({x}, 30), 1)))",
    "arrayconcat":       lambda x: f"length(arrayConcat(arraySlice(acc.2, 1, plus(bitAnd({x}, 30), 1)), acc.2))",
    "arraypushback":     lambda x: f"arrayElement(arrayPushBack(acc.2, toUInt32(bitAnd({x}, 4294967295))), 32)",
    "map_op":            lambda x: f"arrayElement(map(1, {x}), 1)",

    # -- SIBLING placement: the construct does NOT consume the chain; it is
    # a second argument to a compilable parent whose other argument is the
    # chain. This is the OTHER half of the poisoning question.
    "sib_tupleelem":     lambda x: f"plus({x}, acc.3)",
    "sib_arrayelem":     lambda x: f"plus({x}, arrayElement(acc.2, 7))",
    "sib_arraylength":   lambda x: f"plus({x}, length(acc.2))",
    "sib_intdiv":        lambda x: f"plus({x}, intDiv(acc.3, 3))",
}


def _nc(x):
    """A definitively non-compilable node, IN PATH, referencing `x` ONCE."""
    return f"arrayElement(acc.2, plus(bitAnd({x}, 30), 1))"


def fragmented(links, every):
    """`links` compilable links total, with a non-compilable node inserted
    after every `every` of them. `every = 0` means none."""
    x = "toUInt64(acc.3)"
    for k in range(links):
        x = chain(x, 1, start=k)
        if every and (k + 1) % every == 0 and (k + 1) < links:
            x = _nc(x)
    return x


def build_sink(variant, links):
    half = links // 2
    if variant.startswith("frag_"):
        e = variant[len("frag_"):]
        return fragmented(links, 0 if e == "none" else int(e))
    if variant.startswith("chainlen_"):
        return chain("toUInt64(acc.3)", int(variant[len("chainlen_"):]))
    if variant == "nc_top":       # non-compilable node at the ROOT
        return _nc(chain("toUInt64(acc.3)", links))
    if variant == "nc_bot":       # non-compilable node at the LEAF
        return chain(_nc("toUInt64(acc.3)"), links)
    if variant not in CONSTRUCTS:
        raise SystemExit(f"unknown variant: {variant}")
    return chain(CONSTRUCTS[variant](chain("toUInt64(acc.3)", half)), links - half)


# The `rw_N` family: N copies of fold.py's actual per-step register-file
# rewrite -- `arrayConcat(arraySlice(regs, 1, rd-1), [v], arraySlice(regs,
# rd+1))` -- placed in the accumulator's array slot, on top of the usual
# compilable chain in the sink. arrayConcat/arraySlice are non-compilable and
# allocate a fresh 31-element array every step, so this prices JIT-IMMUNE
# work directly: it is the same expression production runs once per
# instruction. Length is preserved at 31 ((rd-1) + 1 + (31-rd)), so the
# accumulator's type and size are stable across iterations.
def regs_expr(n):
    x = "acc.2"
    for j in range(n):
        rd = f"plus(bitAnd(plus(acc.1, {j}), 30), 1)"
        v = f"toUInt32(bitAnd(plus(acc.3, {j}), 4294967295))"
        x = (f"arrayConcat(arraySlice({x}, 1, minus({rd}, 1)), [{v}],"
             f" arraySlice({x}, plus({rd}, 1)))")
    return x


def build_query(variant, k, jit=1, min_count=0, links=24):
    global SALT
    SALT = int(hashlib.sha256(f"{variant}/{EPOCH}".encode()).hexdigest()[:6], 16) * 4096
    regs = "acc.2"
    if variant.startswith("rw_"):
        n = variant[len("rw_"):]
        regs = regs_expr(0 if n == "none" else int(n))
        variant_sink = "chain_only"
    else:
        variant_sink = variant
    if variant_sink == "floor":
        sink = "toUInt32(plus(acc.3, 1))"
    else:
        sink = f"toUInt32(bitAnd({build_sink(variant_sink, links)}, 4294967295))"
    step = f"tuple(toUInt32(plus(acc.1, 4)), {regs}, {sink})"
    return f"""WITH
  (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw))
     FROM (SELECT * FROM {DB}.decoded ORDER BY word_addr)) AS DEC,
  tuple(toUInt32({RAM_BASE}),
        arrayMap(j -> toUInt32(j * 2654435761), range(1, {NREGS + 1})),
        toUInt32(1)) AS INIT
SELECT r.1 AS pc, r.3 AS sink
FROM (SELECT arrayFold((acc, i) -> {step}, range({k}), INIT) AS r)
SETTINGS max_threads = 1,
         compile_expressions = {jit},
         min_count_to_compile_expression = {min_count},
         max_ast_elements = 4000000,
         max_expanded_ast_elements = 4000000"""


ALL_VARIANTS = (["floor"] + list(CONSTRUCTS) + ["nc_top", "nc_bot"]
                + [f"frag_{e}" for e in ("none", 1, 2, 3, 4, 6, 8, 12)]
                # chainlen_256+ trips the SERVER's TOO_DEEP_RECURSION guard
                # (8 MiB analyzer stack), which is not a setting. 128 links
                # (~514 compilable function nodes) is enough for the
                # compile-time-vs-island-size calibration.
                + [f"chainlen_{n}" for n in (1, 2, 4, 8, 16, 32, 64, 128)]
                + [f"rw_{n}" for n in ("none", 1, 2, 4)])

if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--list":
        print(" ".join(ALL_VARIANTS))
        raise SystemExit(0)
    print(build_query(sys.argv[1],
                      int(sys.argv[2]) if len(sys.argv) > 2 else 200_000,
                      int(sys.argv[3]) if len(sys.argv) > 3 else 1,
                      int(sys.argv[4]) if len(sys.argv) > 4 else 0,
                      int(sys.argv[5]) if len(sys.argv) > 5 else 24))
