#!/usr/bin/env python3
"""Baseline: decode-in-the-lambda RV32IM arrayFold step (Phase 0, SPEC §9).

This is the obvious implementation -- fetch the word, pull the fields apart
with bit ops, dispatch on opcode/funct3/funct7 -- and it exists only as the
control for ADR-0002. Compare it against fold_predecoded.py to see what
moving decode into a table is worth.

Usage: fold_naive.py K {nobind|bind|splice}   (register-write strategy;
       measured identical, kept so the claim is reproducible)
"""
import sys

BASE = 0x80000000

# ---- decode fields, expressed against bound instruction word `w` ----
OP   = "bitAnd(w,127)"
RD   = "bitAnd(bitShiftRight(w,7),31)"
F3   = "bitAnd(bitShiftRight(w,12),7)"
RS1  = "bitAnd(bitShiftRight(w,15),31)"
RS2  = "bitAnd(bitShiftRight(w,20),31)"
F7   = "bitShiftRight(w,25)"
A    = f"acc.2[{RS1}+1]"          # rs1 value (UInt32)
B    = f"acc.2[{RS2}+1]"          # rs2 value (UInt32)
SA   = f"toInt32({A})"            # signed rs1
SB   = f"toInt32({B})"            # signed rs2
SHAMT= f"bitAnd({B},31)"
SHI  = f"bitAnd(bitShiftRight(w,20),31)"   # shamt for immediate shifts

# sign-extended immediates
IIMM = "toUInt32(toInt32(bitShiftRight(w,20)) - if(bitAnd(w,2147483648)!=0,4096,0))"
SIMM = ("toUInt32(toInt32(bitOr(bitShiftRight(bitAnd(w,4294967295),25)*32, bitAnd(bitShiftRight(w,7),31)))"
        " - if(bitAnd(w,2147483648)!=0,4096,0))")
UIMM = "toUInt32(bitAnd(w,4294963200))"
# B-type: imm[12|10:5|4:1|11]
BIMM = ("toUInt32(toInt32("
        "bitAnd(bitShiftRight(w,7),30)"                      # imm[4:1] << 1
        "+ bitAnd(bitShiftRight(w,20),2016)"                 # imm[10:5] << 5
        "+ bitShiftLeft(bitAnd(bitShiftRight(w,7),1),11)"    # imm[11]
        ") - if(bitAnd(w,2147483648)!=0,4096,0))")
# J-type: imm[20|10:1|11|19:12]
JIMM = ("toUInt32(toInt32("
        "bitAnd(bitShiftRight(w,20),2046)"                   # imm[10:1] << 1
        "+ bitShiftLeft(bitAnd(bitShiftRight(w,20),1),11)"   # imm[11]
        "+ bitAnd(w,1044480)"                                # imm[19:12]
        ") - if(bitAnd(w,2147483648)!=0,1048576,0))")

def ram_read(addr):
    """Read a word: write-log first (reverse order), then the RAM constant.

    The word index is masked to the 24 MiB window: the benchmark ROM is
    pseudo-random bytes, so decoded branch targets are wild. Masking keeps
    every access in range without changing the work done per instruction
    (one arrayLastIndex over the log + one arrayElement).
    """
    wa = f"bitAnd(bitShiftRight(toUInt32({addr}), 2), 6291455)"
    return (f"if(arrayLastIndex(z -> z = {wa}, acc.3) > 0,"
            f" acc.4[arrayLastIndex(z -> z = {wa}, acc.3)],"
            f" RAM[{wa} + 1])")

# ---- ALU result for OP-IMM (0x13) ----
ALUI = (f"multiIf("
        f"{F3}=0, toUInt32({A} + {IIMM}),"                        # addi
        f"{F3}=1, toUInt32(bitShiftLeft({A}, {SHI})),"            # slli
        f"{F3}=2, toUInt32({SA} < toInt32({IIMM})),"              # slti
        f"{F3}=3, toUInt32({A} < {IIMM}),"                        # sltiu
        f"{F3}=4, bitXor({A}, {IIMM}),"                           # xori
        f"{F3}=5, if({F7}=32, toUInt32(bitShiftRight({SA}, {SHI})), bitShiftRight({A}, {SHI})),"
        f"{F3}=6, bitOr({A}, {IIMM}),"                            # ori
        f"bitAnd({A}, {IIMM}))")                                  # andi

# ---- ALU result for OP (0x33), incl. M-extension ----
ALUR = (f"multiIf("
        f"{F7}=1 AND {F3}=0, toUInt32({SA} * {SB}),"                                    # mul
        f"{F7}=1 AND {F3}=1, toUInt32(bitShiftRight(toInt64({SA}) * toInt64({SB}), 32)),"# mulh
        f"{F7}=1 AND {F3}=2, toUInt32(bitShiftRight(toInt64({SA}) * toInt64({B}), 32))," # mulhsu
        f"{F7}=1 AND {F3}=3, toUInt32(bitShiftRight(toUInt64({A}) * toUInt64({B}), 32))," # mulhu
        f"{F7}=1 AND {F3}=4, if({SB}=0, 4294967295, toUInt32(intDiv({SA}, {SB}))),"      # div
        f"{F7}=1 AND {F3}=5, if({B}=0, 4294967295, toUInt32(intDiv({A}, {B}))),"         # divu
        f"{F7}=1 AND {F3}=6, if({SB}=0, {A}, toUInt32(modulo({SA}, {SB}))),"             # rem
        f"{F7}=1 AND {F3}=7, if({B}=0, {A}, toUInt32(modulo({A}, {B}))),"                # remu
        f"{F3}=0, if({F7}=32, toUInt32({A} - {B}), toUInt32({A} + {B})),"                # add/sub
        f"{F3}=1, toUInt32(bitShiftLeft({A}, {SHAMT})),"                                 # sll
        f"{F3}=2, toUInt32({SA} < {SB}),"                                                # slt
        f"{F3}=3, toUInt32({A} < {B}),"                                                  # sltu
        f"{F3}=4, bitXor({A}, {B}),"                                                     # xor
        f"{F3}=5, if({F7}=32, toUInt32(bitShiftRight({SA}, {SHAMT})), bitShiftRight({A}, {SHAMT})),"
        f"{F3}=6, bitOr({A}, {B}),"
        f"bitAnd({A}, {B}))")

LOAD_ADDR = f"toUInt32({A} + {IIMM})"
LW = ram_read(LOAD_ADDR)
# byte/half extraction from the containing word
LOADV = (f"multiIf("
         f"{F3}=2, toUInt32({LW}),"
         f"{F3}=0, toUInt32(toInt32(bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),255)) - if(bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),128)!=0,256,0)),"
         f"{F3}=4, bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),255),"
         f"{F3}=1, toUInt32(toInt32(bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),65535)) - if(bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),32768)!=0,65536,0)),"
         f"bitAnd(bitShiftRight({LW}, 8*bitAnd({LOAD_ADDR},3)),65535))")

# ---- value written to rd (0 when the instruction writes no register) ----
RESULT = (f"multiIf("
          f"{OP}=19, {ALUI},"
          f"{OP}=51, {ALUR},"
          f"{OP}=3,  {LOADV},"
          f"{OP}=55, {UIMM},"                                   # lui
          f"{OP}=23, toUInt32(acc.1 + {UIMM}),"                 # auipc
          f"{OP}=111, toUInt32(acc.1 + 4),"                     # jal
          f"{OP}=103, toUInt32(acc.1 + 4),"                     # jalr
          f"toUInt32(0))")

BRANCH_TAKEN = (f"multiIf("
                f"{F3}=0, {A} = {B},"
                f"{F3}=1, {A} != {B},"
                f"{F3}=4, {SA} < {SB},"
                f"{F3}=5, {SA} >= {SB},"
                f"{F3}=6, {A} < {B},"
                f"{A} >= {B})")

NEXT_PC = (f"multiIf("
           f"{OP}=111, toUInt32(acc.1 + {JIMM}),"
           f"{OP}=103, toUInt32(bitAnd(toUInt32({A} + {IIMM}), 4294967294)),"
           f"{OP}=99, if({BRANCH_TAKEN}, toUInt32(acc.1 + {BIMM}), toUInt32(acc.1 + 4)),"
           f"toUInt32(acc.1 + 4))")

STORE_ADDR = f"toUInt32({A} + {SIMM})"
IS_STORE = f"{OP}=35"
WRITES_RD = f"({OP}!=35 AND {OP}!=99 AND {RD}!=0)"

def step(variant: str):
    """Build the fold lambda body. `w` is bound; `variant` picks the reg-write strategy."""
    if variant == "splice":
        # Evaluate RESULT exactly once, then rebuild regs from two slices.
        regs_expr = (f"if({WRITES_RD},"
                     f" arrayConcat(arraySlice(acc.2, 1, {RD}), [toUInt32({RESULT})], arraySlice(acc.2, {RD}+2)),"
                     f" acc.2)")
    elif variant == "bind":
        regs_new = ("arrayMap(r -> arrayMap(j -> toUInt32(if(j = " + RD + "+1, r, acc.2[j])), range(1,33)),"
                    " [toUInt32(if(" + WRITES_RD + ", " + RESULT + ", toUInt32(0)))])[1]")
        regs_expr = f"if({WRITES_RD}, {regs_new}, acc.2)"
    else:
        regs_expr = (f"if({WRITES_RD},"
                     f" arrayMap(j -> toUInt32(if(j = {RD}+1, {RESULT}, acc.2[j])), range(1,33)),"
                     f" acc.2)")
    return (
        "tuple("
        f"{NEXT_PC},"
        f"{regs_expr},"
        f"if({IS_STORE}, arrayPushBack(acc.3, toUInt32(bitAnd(bitShiftRight({STORE_ADDR}, 2), 6291455))), acc.3),"
        f"if({IS_STORE}, arrayPushBack(acc.4, toUInt32({B})), acc.4)"
        ")")

def query(K, variant):
    body = step(variant)
    fetch = ram_read("acc.1")
    return f"""WITH (SELECT groupArray(value) FROM (SELECT value FROM clickdoom_bench.ram FINAL ORDER BY word_addr)) AS RAM
SELECT (arrayFold(
  (acc, i) -> arrayMap(w -> {body}, [{fetch}])[1],
  range({K}),
  tuple(toUInt32({BASE}), arrayResize(emptyArrayUInt32(), 32, toUInt32(0)), emptyArrayUInt32(), emptyArrayUInt32())
) AS r).1, length(r.3) SETTINGS max_threads=1"""

if __name__ == "__main__":
    K = int(sys.argv[1]); variant = sys.argv[2]
    print(query(K, variant))
