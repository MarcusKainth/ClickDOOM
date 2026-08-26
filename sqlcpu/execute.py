#!/usr/bin/env python3
"""RV32I execute expression and register file — sqlcpu workstream, issue #19.

Generates the SQL expression that computes one instruction's effect, given a
decoded row (sqlcpu/schema.sql's `decoded` columns) and the current register
file. This is *not* a runnable query on its own — like the Phase 0 bench's
fold_predecoded.py, it emits SQL text meant to sit inside `executor`'s
arrayFold lambda (#23) or, for testing here, inside a single-row SELECT
(sqlcpu/test_execute.py does the latter). PURITY.md's posture is unchanged
either way: this module computes nothing itself, it only builds the SQL text
that ClickHouse evaluates.

Scope and the interface with `executor`:
  * M-extension (decoded.id 10..17) is issue #20's arms, not this one's —
    RESULT leaves them as a placeholder (0) rather than reaching ahead into
    that issue's scope. #20 replaces just that multiIf branch.
  * Loads need the raw word at the computed address, which requires
    checking the write-log before RAM (ADR-0001) — that's executor's memory
    model (#23), not this module's. `alu_result()` takes the loaded word as
    a parameter (`loaded_word_expr`) rather than reading memory itself.
  * SELF_MODIFY (SPEC §1: a store into the text region is a fatal halt) is
    also out of scope here: detecting it needs the text region's bounds,
    which are only known where stores actually land (executor's write-log
    commit, #23/#25), not in a per-instruction expression that has no access
    to that context. This module emits the store's address and value;
    executor's commit path is where SELF_MODIFY gets checked.
  * Halt detection for the two decode-time sentinels (id 254 = ecall/ebreak/
    CSR, id 255 = illegal) IS in scope: decode already knows these need a
    fatal halt (SPEC §1), so surfacing it here means executor's batch loop
    doesn't need to re-inspect the raw instruction word to find out.

Register file convention (matches sqlcpu/schema.sql): `regs` is a 31-element
Array(UInt32), 1-indexed, regs[r] = x_r's value for r in 1..31. x0 (r = 0)
is never stored; every register read/write below goes through an explicit
r = 0 check rather than relying on an out-of-range array access.

PC convention: `pcidx` is a WORD index (byte address >> 2), the same domain
as `decoded.word_addr`/`decoded.tgt` and `ram.word_addr` — not the byte
address SPEC §5 stores in `cpu_state.pc`. Converting between the two
(`pcidx * 4` / `pc >> 2`) happens once at the accumulator's boundary
(load and commit), executor's concern (#23/#25); working in word units
inside the step avoids a shift on every single instruction. The one
value that must NOT stay in word units is the jal/jalr link value written
to rd — that becomes a real data value a program dereferences, so it is
always computed as a byte address.
"""

# ---- register read/write ---------------------------------------------------

def reg_read(reg_expr: str, regs_expr: str = "regs") -> str:
    """Value of register `reg_expr` (0..31), x0 hardwired to 0."""
    return f"if(({reg_expr}) = 0, toUInt32(0), {regs_expr}[{reg_expr}])"


def regs_write(rd_expr: str, value_expr: str, regs_expr: str = "regs") -> str:
    """New 31-element regs array with x_rd set to value_expr, x0 writes discarded."""
    return (
        f"if(({rd_expr}) != 0, "
        f"arrayConcat(arraySlice({regs_expr}, 1, ({rd_expr}) - 1), "
        f"[toUInt32({value_expr})], "
        f"arraySlice({regs_expr}, ({rd_expr}) + 1)), "
        f"{regs_expr})"
    )


# ---- operands ---------------------------------------------------------------
# A = rs1's value. B = rs2's value + imm: the R/I-type collapse from ADR-0002
# (decode sets rs2 = 0 and imm = 0 respectively, whichever the encoding
# doesn't carry) makes this the correct second operand either way.

def operand_a(rs1="rs1", regs="regs") -> str:
    return reg_read(rs1, regs)


def operand_b(rs2="rs2", imm="imm", regs="regs") -> str:
    return f"toUInt32({reg_read(rs2, regs)} + {imm})"


# ---- loaded-value extraction --------------------------------------------
# Byte/half/word extraction from the containing word, driven by decode's
# pre-decoded width mask (mk) and sign flag (sg) — one arm regardless of
# lb/lh/lw/lbu/lhu (ADR-0002).

def load_value(loaded_word_expr: str, addr_expr: str, mk="mk", sg="sg") -> str:
    shift = f"(8 * bitAnd({addr_expr}, 3))"
    extracted = f"bitAnd(bitShiftRight({loaded_word_expr}, {shift}), {mk})"
    return (
        f"toUInt32({extracted} - if(bitAnd({extracted}, bitShiftRight({mk}, 1) + 1) != 0 "
        f"AND {sg} != 0, toUInt64({mk}) + 1, 0))"
    )


# ---- the result written to rd (RV32I arms; M-extension is #20) -------------

def alu_result(loaded_word_expr: str, addr_expr: str, id_="id", a="A", b="B",
                mk="mk", sg="sg", pcidx="pcidx") -> str:
    lv = load_value(loaded_word_expr, addr_expr, mk, sg)
    return (
        "multiIf("
        f"{id_} = 0, toUInt32({a} + {b}),"
        f"{id_} = 1, toUInt32({a} - {b}),"
        f"{id_} = 2, toUInt32(bitShiftLeft({a}, bitAnd({b}, 31))),"
        f"{id_} = 3, toUInt32(toInt32({a}) < toInt32({b})),"
        f"{id_} = 4, toUInt32({a} < {b}),"
        f"{id_} = 5, bitXor({a}, {b}),"
        f"{id_} = 6, toUInt32(bitShiftRight({a}, bitAnd({b}, 31))),"
        f"{id_} = 7, toUInt32(bitShiftRight(toInt32({a}), bitAnd({b}, 31))),"
        f"{id_} = 8, bitOr({a}, {b}),"
        f"{id_} = 9, bitAnd({a}, {b}),"
        f"{id_} >= 10 AND {id_} <= 17, toUInt32(0),"  # M-extension: issue #20
        f"{id_} = 18, {lv},"
        f"toUInt32({pcidx} * 4 + 4))"  # default arm: jal/jalr link value (byte address)
    )


# ---- next pc (word index) ---------------------------------------------------

def next_pc(id_="id", a="A", b="B", tgt="tgt", imm="imm", pcidx="pcidx") -> str:
    jalr_target = f"bitShiftRight(bitAnd(toUInt32({a} + {imm}), 4294967294), 2)"
    return (
        "multiIf("
        f"{id_} = 20, if({a} = {b}, {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 21, if({a} != {b}, {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 22, if(toInt32({a}) < toInt32({b}), {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 23, if(toInt32({a}) >= toInt32({b}), {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 24, if({a} < {b}, {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 25, if({a} >= {b}, {tgt}, toUInt32({pcidx} + 1)),"
        f"{id_} = 26, {tgt},"
        f"{id_} = 27, toUInt32({jalr_target}),"
        f"toUInt32({pcidx} + 1))"
    )


# ---- store address/value (word_addr domain; SELF_MODIFY check is executor's) --

def is_store(id_="id") -> str:
    return f"{id_} = 19"


def store_word_addr(a="A", imm="imm") -> str:
    # decoded.imm already carries the store's SIMM offset (schema.sql).
    return f"bitShiftRight(toUInt32({a} + {imm}), 2)"


def store_value(loaded_word_expr: str, addr_expr: str, b="B", mk="mk") -> str:
    # Read-modify-write over the containing word: keep the bytes outside the
    # mask, splice in the low bits of B at the address's byte offset.
    shift = f"(8 * bitAnd({addr_expr}, 3))"
    return (
        f"toUInt32(bitOr("
        f"bitAnd({loaded_word_expr}, bitXor(4294967295, toUInt32(bitShiftLeft({mk}, {shift})))),"
        f"toUInt32(bitShiftLeft(bitAnd({b}, {mk}), {shift}))))"
    )


# ---- halt detection (decode-time sentinels only; SELF_MODIFY is executor's) --

def halted(id_="id") -> str:
    return f"toUInt8({id_} = 254 OR {id_} = 255)"


def halt_reason(id_="id") -> str:
    return f"if({id_} = 254, 'ECALL_EBREAK_CSR', if({id_} = 255, 'ILLEGAL_INSN', ''))"
