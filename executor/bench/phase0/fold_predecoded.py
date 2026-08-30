#!/usr/bin/env python3
"""Generate the pre-decoded arrayFold step (ADR-0002) for the baseline fold benchmark.

Why this file exists
--------------------
The baseline benchmark measured that an arrayFold step costs roughly 0.8 us per *expression
node* in the lambda, essentially independent of how much data those nodes
touch. Node count, not data volume, is the throughput lever. So this variant
moves every decode bit-op out of the lambda into a table built inside
ClickHouse (PURITY.md explicitly allows decoding the ROM *in* the database)
and collapses the opcode space so the execute multiIf has as few arms as
possible.

Two collapses are worth calling out, because they look like tricks:

  * I-type vs R-type. The decoder sets rs2=0 for I-type (x0 is hardwired 0)
    and imm=0 for R-type, so `b = regs[rs2] + imm` yields the right second
    operand for both with no branch at all. addi and add become one arm.
  * lui / auipc / jal-link. All three are "put a constant in rd", and the
    constant is static per pc, so the decoder precomputes it and they become
    the same `add` arm with rs1=rs2=0.

Usage:  fold_predecoded.py K [e2e]
        K      instructions per batch
        e2e    emit the full batch INSERT (state reload + fold + staging)
               instead of a bare SELECT
"""
import sys

DB   = "clickdoom_bench"
NW   = 6291456          # RAM words (24 MiB)
MASK = NW - 1
DECN = 524288           # pre-decoded text-segment words (2 MiB)
DECM = DECN - 1

# accumulator: (pcidx UInt32, regs Array(UInt32)[32], wl_addr, wl_val)
PC  = "acc.1"
IDX = "(acc.1 + 1)"     # 1-based index into the decode arrays
# DEC[i] is a tuple(id, rd, rs1, rs2, imm, tgt, mk, sg) -- one combined
# groupArray per table, not one per column. See WITH_CLAUSE's comment below
# for why: sqlcpu found (PR #67, executor/fold.py) that ClickHouse 26.3's
# `optimize_read_in_order` can silently misalign a per-column groupArray
# against its declared ORDER BY. Not reproduced against this bench's own
# tables (team lead verified 0/524,288 and 0/6,291,456 misaligned, both
# directions), so the baseline benchmark's ratified numbers stand unchanged -- this is a
# defensive fix, not a correction, applied because this file is the pattern
# both executor/fold.py and sqlcpu's schema.sql originally copied from.
ID  = f"DEC[{IDX}].1"
RD  = f"DEC[{IDX}].2"
IMM = f"DEC[{IDX}].5"
TGT = f"DEC[{IDX}].6"
DMK = f"DEC[{IDX}].7"
DSG = f"DEC[{IDX}].8"

A   = f"acc.2[DEC[{IDX}].3 + 1]"
B   = f"toUInt32(acc.2[DEC[{IDX}].4 + 1] + {IMM})"
SA  = f"toInt32({A})"
SB  = f"toInt32({B})"

ADDR = f"toUInt32({A} + {IMM})"
WA   = f"bitAnd(bitShiftRight({ADDR}, 2), {MASK})"
# Read a word: write-log first (reverse order, last writer wins), then RAM.
# RAMT[i].1 -- RAM is captured as a one-column tuple too, same defensive
# pattern (a lone-column groupArray was never the vulnerable shape, but this
# keeps RAM and DEC visibly consistent with each other and with fold.py).
LW   = (f"if(arrayLastIndex(z -> z = {WA}, acc.3) > 0,"
        f" acc.4[arrayLastIndex(z -> z = {WA}, acc.3)], RAMT[{WA} + 1].1)")
SH   = f"(8 * bitAnd({ADDR}, 3))"

# One load path for lb/lh/lw/lbu/lhu: extract with the pre-decoded width mask,
# then subtract mask+1 when the pre-decoded sign bit is set (sg=0 => unsigned).
LOADV = (f"toUInt32(bitAnd(bitShiftRight({LW}, {SH}), {DMK})"
         f" - if(bitAnd(bitAnd(bitShiftRight({LW}, {SH}), {DMK}), {DSG}) != 0,"
         f" toUInt64({DMK}) + 1, 0))")

RESULT = ("multiIf("
  f"{ID}=0, toUInt32({A} + {B}),"
  f"{ID}=1, toUInt32({A} - {B}),"
  f"{ID}=2, toUInt32(bitShiftLeft({A}, bitAnd({B},31))),"
  f"{ID}=3, toUInt32({SA} < {SB}),"
  f"{ID}=4, toUInt32({A} < {B}),"
  f"{ID}=5, bitXor({A}, {B}),"
  f"{ID}=6, toUInt32(bitShiftRight({A}, bitAnd({B},31))),"
  f"{ID}=7, toUInt32(bitShiftRight({SA}, bitAnd({B},31))),"
  f"{ID}=8, bitOr({A}, {B}),"
  f"{ID}=9, bitAnd({A}, {B}),"
  f"{ID}=10, toUInt32({SA} * {SB}),"
  f"{ID}=11, toUInt32(bitShiftRight(toInt64({SA}) * toInt64({SB}), 32)),"
  f"{ID}=12, toUInt32(bitShiftRight(toInt64({SA}) * toInt64({B}), 32)),"
  f"{ID}=13, toUInt32(bitShiftRight(toUInt64({A}) * toUInt64({B}), 32)),"
  f"{ID}=14, if({SB}=0, 4294967295, toUInt32(intDiv({SA}, {SB}))),"
  f"{ID}=15, if({B}=0, 4294967295, toUInt32(intDiv({A}, {B}))),"
  f"{ID}=16, if({SB}=0, {A}, toUInt32(modulo({SA}, {SB}))),"
  f"{ID}=17, if({B}=0, {A}, toUInt32(modulo({A}, {B}))),"
  f"{ID}=18, {LOADV},"
  f"{TGT})")            # jal/jalr link value, precomputed by the decoder

NEXT = ("multiIf("
  f"{ID}=20, if({A} = {B},  {TGT}, toUInt32({PC}+1)),"
  f"{ID}=21, if({A} != {B}, {TGT}, toUInt32({PC}+1)),"
  f"{ID}=22, if({SA} < {SB},  {TGT}, toUInt32({PC}+1)),"
  f"{ID}=23, if({SA} >= {SB}, {TGT}, toUInt32({PC}+1)),"
  f"{ID}=24, if({A} < {B},  {TGT}, toUInt32({PC}+1)),"
  f"{ID}=25, if({A} >= {B}, {TGT}, toUInt32({PC}+1)),"
  f"{ID}=26, {TGT},"
  f"{ID}=27, toUInt32(bitAnd(bitShiftRight(toUInt32({A} + {IMM}), 2), {DECM})),"
  f"toUInt32(bitAnd({PC}+1, {DECM})))")

# sb/sh/sw share one read-modify-write path over the containing word.
SVAL = (f"toUInt32(bitOr(bitAnd({LW}, bitXor(4294967295, toUInt32(bitShiftLeft({DMK}, {SH})))),"
        f" toUInt32(bitShiftLeft(bitAnd({B}, {DMK}), {SH}))))")


def step():
    is_store = f"{ID}=19"
    return ("tuple("
            f"{NEXT},"
            f"if({RD} != 0,"
            f" arrayConcat(arraySlice(acc.2, 1, {RD}), [toUInt32({RESULT})], arraySlice(acc.2, {RD}+2)),"
            f" acc.2),"
            f"if({is_store}, arrayPushBack(acc.3, {WA}), acc.3),"
            f"if({is_store}, arrayPushBack(acc.4, {SVAL}), acc.4))")


# One combined groupArray(tuple(...)) per table, not one groupArray per
# column -- see the ID/RD/... comment above. `SETTINGS optimize_read_in_order
# = 0` would also fix it but is a setting a future query can omit, not a
# structural fix; capturing every column of a table in ONE tuple, in ONE
# groupArray call, can't misalign columns against each other because
# there's no second independent read path for the optimizer to diverge onto.
WITH_CLAUSE = f"""
  (SELECT groupArray(tuple(value))
     FROM (SELECT value, word_addr FROM {DB}.ram FINAL ORDER BY word_addr)) AS RAMT,
  (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg))
     FROM (SELECT id, rd, rs1, rs2, imm, tgt, mk, sg, widx
           FROM {DB}.decoded ORDER BY widx)) AS DEC"""


def select_only(K):
    """Fold in isolation: no state reload, no commit. Measures the crank alone."""
    return f"""WITH{WITH_CLAUSE}
SELECT (arrayFold((acc, i) -> {step()}, range({K}),
  tuple(toUInt32(0), arrayResize(emptyArrayUInt32(), 32, toUInt32(0)),
        emptyArrayUInt32(), emptyArrayUInt32())) AS r).1, length(r.3)
SETTINGS max_threads = 1"""


def batch(K):
    """One full batch: reload prior state, fold K instructions, stage the result.

    assumeNotNull/CAST are load-bearing: a scalar subquery is Nullable, and a
    Nullable in the initial accumulator makes arrayFold reject the lambda for
    returning a non-Nullable tuple.
    """
    return f"""INSERT INTO {DB}.batch_out
WITH{WITH_CLAUSE},
  (SELECT tuple(batch_id, pcidx, regs, icount)
     FROM {DB}.state ORDER BY batch_id DESC LIMIT 1) AS PREV
SELECT toUInt64(assumeNotNull(PREV.1) + 1)   AS batch_id,
       toUInt64(assumeNotNull(PREV.4) + {K}) AS icount,
       (arrayFold((acc, i) -> {step()}, range({K}),
          tuple(assumeNotNull(PREV.2), CAST(PREV.3, 'Array(UInt32)'),
                emptyArrayUInt32(), emptyArrayUInt32())) AS r).1 AS pcidx,
       r.2 AS regs, r.3 AS wl_addr, r.4 AS wl_val
SETTINGS max_threads = 1"""


if __name__ == "__main__":
    K = int(sys.argv[1])
    print(batch(K) if len(sys.argv) > 2 and sys.argv[2] == "e2e" else select_only(K))
