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
  * Halt detection for decode's four sentinels (id 28 = ecall, 29 = ebreak,
    30 = csr, 31 = illegal) IS in scope: decode already knows these need a
    fatal halt (SPEC §1), so surfacing it here means executor's batch loop
    doesn't need to re-inspect the raw instruction word to find out.
  * MISALIGNED (SPEC §1) IS in scope, checked eagerly, here — the team
    lead's ruling on #37 overrides refemu's original "fault surfaces on
    the next fetch" proposal (and this module's own earlier docstring,
    which deferred to that): the RISC-V ISA reports instruction-address-
    misaligned on the branch/jump that computes the bad target, not on the
    target. Concretely: for a taken branch, jal, or jalr whose target has
    bit 1 set (2-byte aligned but not 4-byte aligned — only bit 0 is
    forced to 0 by the encodings), the instruction halts with MISALIGNED,
    its own pc as the halt record's pc (not the target), and neither pc
    nor rd is updated — matching refemu's PR #51 exactly, including "not
    taken" never faulting even if the untaken target would have been bad.
    This is checkable here specifically because it's eager: the check
    only needs values this expression already has (id, A, B, tgt, imm,
    pc), not anything from a future fetch. What made it representable at
    all is still the fix noted below: an earlier word-indexed `pc`
    convention would have silently discarded the very bit this check
    tests.

Register file convention (matches sqlcpu/schema.sql): `regs` is a 31-element
Array(UInt32), 1-indexed, regs[r] = x_r's value for r in 1..31. x0 (r = 0)
is never stored; every register read/write below goes through an explicit
r = 0 check rather than relying on an out-of-range array access.

PC convention: `pc` is the BYTE address, matching `cpu_state.pc` (SPEC §5)
and `decoded.tgt` directly — no unit conversion at the accumulator boundary.
An earlier version of this module used a word-indexed `pcidx` internally for
one shift fewer per step; abandoned because it cannot represent a misaligned
byte address at all, which a MISALIGNED check needs to be able to see.
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
                mk="mk", sg="sg", pc="pc") -> str:
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
        f"toUInt32({pc} + 4))"  # default arm: jal/jalr link value (byte address)
    )


# ---- MISALIGNED (SPEC §1, eager per #37) ------------------------------------
# is_misaligned() is used by next_pc(), halted(), halt_reason() and
# rd_or_suppressed() below -- all four take an optional `misaligned=`
# parameter so a caller can bind this expression once (e.g. a single
# `(is_misaligned(...)) AS misaligned` in the surrounding WITH clause) and
# pass the alias through, instead of each of the four re-deriving the same
# ~9-arm multiIf independently. Not just style: four full inline copies in
# one lambda is a real node-count cost under Phase 0's per-node model
# (ADR-0002), and it's what first surfaced this to me -- sqlcpu/test_execute.py
# hit ClickHouse's `AST is too big` query limit before this parameter existed,
# from ~56 test rows each carrying four copies of the same expression.

def _jalr_target(a="A", imm="imm") -> str:
    return f"bitAnd(toUInt32({a} + {imm}), 4294967294)"


def is_misaligned(id_="id", a="A", b="B", tgt="tgt", imm="imm") -> str:
    """True iff this instruction is a taken branch/jal/jalr whose target
    isn't 4-byte aligned. False (never faults) for every other id,
    including an untaken branch even when its target would have been bad
    (refemu's PR #51: `beq` that doesn't branch never evaluates its own
    target's alignment)."""
    jalr_target = _jalr_target(a, imm)
    return (
        "multiIf("
        f"{id_} = 20, {a} = {b} AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 21, {a} != {b} AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 22, toInt32({a}) < toInt32({b}) AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 23, toInt32({a}) >= toInt32({b}) AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 24, {a} < {b} AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 25, {a} >= {b} AND bitAnd({tgt}, 3) != 0,"
        f"{id_} = 26, bitAnd({tgt}, 3) != 0,"
        f"{id_} = 27, bitAnd({jalr_target}, 3) != 0,"
        "toUInt8(0))"
    )


# ---- next pc (byte address) -------------------------------------------------

def next_pc(id_="id", a="A", b="B", tgt="tgt", imm="imm", pc="pc", misaligned=None) -> str:
    """`misaligned`: pass a WITH-bound alias (e.g. from a single
    `(is_misaligned(...)) AS misaligned` computed once) to reuse across this,
    halted(), halt_reason() and rd_or_suppressed() instead of each
    re-deriving is_misaligned()'s ~9-arm multiIf independently — four full
    copies of it in one lambda is a real node-count cost under Phase 0's
    per-node model (ADR-0002), not just verbose SQL text. Omit it (the
    default) to compute fresh inline, which is what every caller in this
    file's own tests did before this parameter existed."""
    is_misaligned_expr = misaligned if misaligned is not None else is_misaligned(id_, a, b, tgt, imm)
    # A misaligned taken branch/jal/jalr freezes at ITS OWN pc (SPEC §1,
    # eager per #37 — see is_misaligned() and the module docstring): pc
    # never advances, matching refemu's "nothing about the jump takes
    # effect". Checked first since it overrides every arm below, not just
    # the ones that would otherwise transfer control.
    jalr_target = _jalr_target(a, imm)
    return (
        "multiIf("
        f"{is_misaligned_expr}, {pc},"
        f"{id_} = 20, if({a} = {b}, {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 21, if({a} != {b}, {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 22, if(toInt32({a}) < toInt32({b}), {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 23, if(toInt32({a}) >= toInt32({b}), {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 24, if({a} < {b}, {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 25, if({a} >= {b}, {tgt}, toUInt32({pc} + 4)),"
        f"{id_} = 26, {tgt},"
        f"{id_} = 27, toUInt32({jalr_target}),"
        f"toUInt32({pc} + 4))"
    )


# ---- store address/value (word_addr domain; SELF_MODIFY check is executor's) --

def is_store(id_="id") -> str:
    return f"{id_} = 19"


def store_word_addr(a="A", imm="imm") -> str:
    # decoded.imm already carries the store's SIMM offset (schema.sql).
    # This IS shifted to a word index (unlike next_pc/tgt): a store address
    # is where a value lands in `ram`, not a value a program branches to,
    # so there's no MISALIGNED concern the shift could hide -- and `ram` is
    # keyed by word_addr, so executor's write-log needs it in that domain.
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


# ---- halt detection (decode-time sentinels + eager MISALIGNED; ------------
# ---- SELF_MODIFY is executor's, see the module docstring) -----------------

def halted(id_="id", a="A", b="B", tgt="tgt", imm="imm", misaligned=None) -> str:
    is_misaligned_expr = misaligned if misaligned is not None else is_misaligned(id_, a, b, tgt, imm)
    return f"toUInt8(({id_} >= 28 AND {id_} <= 31) OR ({is_misaligned_expr}))"


def halt_reason(id_="id", a="A", b="B", tgt="tgt", imm="imm", misaligned=None) -> str:
    # Vocabulary agreed with refemu, issue #37: ECALL/EBREAK/CSR/ILLEGAL_INSN/
    # MISALIGNED here; SELF_MODIFY is produced elsewhere (see module docstring).
    is_misaligned_expr = misaligned if misaligned is not None else is_misaligned(id_, a, b, tgt, imm)
    return (
        f"multiIf({id_} = 28, 'ECALL', {id_} = 29, 'EBREAK', "
        f"{id_} = 30, 'CSR', {id_} = 31, 'ILLEGAL_INSN', "
        f"{is_misaligned_expr}, 'MISALIGNED', '')"
    )


def rd_or_suppressed(id_="id", a="A", b="B", tgt="tgt", imm="imm", rd="rd", misaligned=None) -> str:
    """`rd`, unless this step is a misaligned taken jump (SPEC §1, #37) — in
    which case rd must not be written at all, and passing this through
    regs_write()'s existing `rd != 0` guard as a stand-in `rd` of 0 is a
    write-suppression, not a real write to x0."""
    is_misaligned_expr = misaligned if misaligned is not None else is_misaligned(id_, a, b, tgt, imm)
    return f"if({is_misaligned_expr}, toUInt8(0), {rd})"


def misaligned_target(id_="id", a="A", tgt="tgt", imm="imm") -> str:
    """The bad (not 4-byte-aligned) target itself — jal/branch use `tgt`
    directly, jalr computes it live. Only meaningful where is_misaligned()
    is true; for executor's halt_extra (mirroring bad_addr/self_modify's
    existing "report the address" convention in their fold.py) rather than
    something this module consumes itself."""
    jalr_target = _jalr_target(a, imm)
    return f"if({id_} = 27, toUInt32({jalr_target}), {tgt})"
