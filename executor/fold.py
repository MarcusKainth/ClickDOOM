#!/usr/bin/env python3
"""The batch fold statement (#23): SPEC §1 halt semantics, SPEC §6 early
termination (halt / write-log high-water mark -- FRAME_COMMIT hooks in at
#24, once MMIO address routing exists), and the write-log versioning fix
(SPEC §5, flagged on PR #30 and in issue #35): every write-log entry carries
its own retiring instruction's icount, not the batch's final icount.

Starting point: executor/bench/phase0/fold_predecoded.py (ADR-0002). Reused:
the collapsed op_id space 0-27 (agreed with sqlcpu, PR #42/#46/#49), the
accumulator's RAM/write-log addressing idiom, the register-write guard on
rd=0. NOT reused, despite looking similar at a glance: the bench's word-
indexed pc and its 32-element x0-pinned register file -- both were bugs the
bench's own "never actually executed" disclaimer let slide (RESULTS.md §6),
caught by sqlcpu and the team lead reviewing this PR against their
schema.sql, and fixed here (see IDX's docstring and the register-file note
below). New in this file relative to the bench:

  * op_id 28-31 (ecall/ebreak/csr/illegal) -- SPEC §1 fatal-halt decode arms,
    agreed with sqlcpu.
  * Address-bounds and load/store-alignment checks (SPEC §1, §2) ahead of
    every memory access, using the "mask to a safe index, check the real
    bound separately" idiom so an out-of-range address can never make
    arrayElement throw, whether or not the access is what triggers the halt.
  * A misaligned jump/branch target (jal/jalr/a taken branch) halts eagerly
    at the transferring instruction, per issue #37's ruling -- neither pc
    nor rd updates. Unreachable from a well-formed RV32IM binary, which is
    why it's covered by a dedicated test rather than left as "it can't
    happen".
  * SELF_MODIFY: a store whose target word index falls in [text_start,
    text_end).
  * A `stopped`/`halted` split in the accumulator: a step that halts does
    NOT retire (pc/regs/write-log freeze at the faulting instruction, per
    "pc ... in the halt record" -- the halt record's pc is the faulter's,
    not the next one); a step that merely crosses the write-log high-water
    mark DOES retire (it is the batch's last instruction, not a fault).

Usage: fold.py K [--hwm N] [--e2e]
  K       instructions per batch
  --hwm N write-log high-water mark (default: config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT)
  --e2e   emit the full batch INSERT against the fixture staging table
          instead of a bare SELECT (see schema_fixture.sql)
"""
import argparse

import config

DB = "clickdoom_executor"


def _addr_and_align(A, IMM, DMKv, RAM_BASE, RAM_WORDS):
    """Returns (ADDR, ADDR64, bad_addr_cond, misaligned_cond, WA_safe).

    WA_safe is masked into [0, RAM_WORDS) unconditionally, so arrayElement
    on RAM/write-log arrays never throws regardless of whether the access is
    actually in range -- bad_addr_cond is the real (unmasked) bounds check
    used for halt detection, computed independently.
    """
    ADDR = f"toUInt32({A} + {IMM})"
    ADDR64 = f"toUInt64({ADDR})"
    ram_end = RAM_BASE + RAM_WORDS * 4
    bad_addr_cond = f"({ADDR64} < {RAM_BASE} OR {ADDR64} >= {ram_end})"
    align_mask = f"multiIf({DMKv}=4294967295, 3, {DMKv}=65535, 1, 0)"
    misaligned_cond = f"(bitAnd({ADDR}, {align_mask}) != 0)"
    # `least(..., RAM_WORDS-1)`, not a bitAnd mask: SPEC's 24 MiB RAM is
    # 6,291,456 words, not a power of two, so a power-of-two mask (Phase 0's
    # trick, fine there since that harness never claimed correctness) would
    # silently under-cover the top of RAM. `least` needs no such assumption
    # -- it only has to clamp into a valid index, which any UInt32 already
    # satisfies. Only used to keep arrayElement in-bounds when the access is
    # about to be judged bad_addr/self_modify anyway; the real bound is
    # bad_addr_cond above, computed independently of this clamp.
    # >>2: byte offset from RAM_BASE to word index. Dropped in an earlier
    # draft of this function -- WA must be a word index, not a byte offset.
    wa_safe = f"least(bitShiftRight(toUInt32(toUInt64({ADDR}) - {RAM_BASE}), 2), {RAM_WORDS - 1})"
    return ADDR, ADDR64, bad_addr_cond, misaligned_cond, wa_safe


def build_step(K, text_start_widx, text_end_widx, decn, ram_words,
               ram_base=config.RAM_BASE, hwm=config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT):
    """Returns the arrayFold lambda body: `(acc, i) -> tuple(...)`.

    Accumulator (5-tuple): pc, regs[31], wl, control, retired, where pc is a
    byte address (matching cpu_state.pc, sqlcpu's schema.sql), regs is
    x1..x31 (no x0 slot), wl = tuple(addr[], val[], icount[]) and control =
    tuple(stopped, halted, halt_reason, halt_pc, halt_extra).

    Deliberately NOT an 11-flat-field tuple (an earlier version of this
    function was exactly that): every accumulator field needs its own
    `if(step_retires/step_halts_now, ..., acc.N)` guard, and Phase 0's
    cost model charges per node regardless of which branch is "taken" --
    so 11 fields means the halt/retire condition gets evaluated ~10 times
    per step. Packing the write-log's three arrays and the halt record's
    five scalars into one tuple field each cuts that to ~2 evaluations,
    measured to matter (see the PR's before/after numbers).
    """
    PC = "acc.1"
    STOPPED = "acc.4.1"

    # PC is a BYTE address (matching cpu_state.pc, SPEC §5's reset value
    # `0x8000_0000`, and sqlcpu's schema.sql/execute.py, PR #46/#49) -- NOT
    # a word index. An earlier version of this function kept PC as a word
    # index for one shift fewer per step; sqlcpu found the same bug
    # independently and migrated first (PR #46/#49), and their review of
    # this PR caught it here too: a word index can't represent a target
    # whose bit 1 is set (2-byte aligned but not 4-byte aligned -- the
    # RV32I encodings only force bit 0 to 0), so a `>>2` doesn't just make
    # a MISALIGNED check harder, it destroys the bit the check needs before
    # any check could run. IDX below is a *safe, clamped* word index used
    # only for array lookups; PC itself is never rounded.
    IDX = f"(least(bitShiftRight(toUInt32(toUInt64({PC}) - {ram_base}), 2), {decn - 1}) + 1)"

    # DEC[i] is a tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw) -- one
    # combined groupArray per table, not one per column. See decode_with()'s
    # comment below for why (sqlcpu, PR #67: optimize_read_in_order can
    # silently misalign a per-column groupArray against word_addr).
    ID = f"DEC[{IDX}].1"
    RD = f"DEC[{IDX}].2"
    IMM = f"DEC[{IDX}].5"
    TGT = f"DEC[{IDX}].6"
    DMKv = f"DEC[{IDX}].7"
    DSGv = f"DEC[{IDX}].8"
    RAW = f"DEC[{IDX}].9"

    # Register file per sqlcpu's schema.sql (PR #42): `regs` is 31 elements,
    # 1-indexed, x1..x31 -- x0 has NO array slot (unlike the Phase 0 bench's
    # 32-element array with x0 pinned at position 1, which this PR initially
    # copied uncritically). Reading x0 must be an explicit 0, not an array
    # lookup; writing x0 must be discarded, not written to some slot.
    R1 = f"DEC[{IDX}].3"
    R2 = f"DEC[{IDX}].4"
    A = f"if({R1} = 0, toUInt32(0), acc.2[{R1}])"
    B = f"toUInt32(if({R2} = 0, toUInt32(0), acc.2[{R2}]) + {IMM})"
    # Raw rs2 (no +imm). B doubles as the ALU/branch second operand (ADR-0002's
    # I-type/R-type collapse relies on +imm there), but a store's *value* is
    # regs[rs2] alone -- imm is the address offset, already spent computing
    # ADDR from A, and must not also land in the stored value. Using B for
    # SVAL here was a latent bug inherited from fold_predecoded.py, caught by
    # this PR's store/load round-trip test.
    RS2V = f"if({R2} = 0, toUInt32(0), acc.2[{R2}])"
    SA = f"toInt32({A})"
    SB = f"toInt32({B})"

    ADDR, ADDR64, bad_addr_cond, misaligned_cond, WA = _addr_and_align(
        A, IMM, DMKv, ram_base, ram_words)
    SH = f"(8 * bitAnd({ADDR}, 3))"

    # Write-log first (reverse order, last writer wins), then RAM.
    # RAMT[i].1 -- see decode_with()'s comment: RAM is captured as a
    # one-column tuple, same defensive pattern as decoded's columns.
    LW = (f"if(arrayLastIndex(z -> z = {WA}, acc.3.1) > 0,"
          f" acc.3.2[arrayLastIndex(z -> z = {WA}, acc.3.1)], RAMT[{WA} + 1].1)")

    # sg is a boolean sign-extend flag (0/1), not a bit-mask value -- matches
    # sqlcpu's schema.sql/execute.py (`sg UInt8`) exactly, per their
    # confirmation on PR #46. The sign bit's *position* is derived from mk
    # itself (mk's top bit: 0xFF -> 0x80, 0xFFFF -> 0x8000) rather than
    # stored as a separate value, so there's one fewer column whose meaning
    # could drift out of sync with mk. An earlier version of this file
    # stored sg as the bit-mask directly, inherited from the Phase 0 bench.
    EXTRACTED = f"bitAnd(bitShiftRight({LW}, {SH}), {DMKv})"
    SIGN_POS = f"(bitShiftRight({DMKv}, 1) + 1)"
    LOADV = (f"toUInt32({EXTRACTED}"
             f" - if(bitAnd({EXTRACTED}, {SIGN_POS}) != 0 AND {DSGv} != 0,"
             f" toUInt64({DMKv}) + 1, 0))")

    SVAL = (f"toUInt32(bitOr(bitAnd({LW}, bitXor(4294967295, toUInt32(bitShiftLeft({DMKv}, {SH})))),"
            f" toUInt32(bitShiftLeft(bitAnd({RS2V}, {DMKv}), {SH}))))")

    # jal/jalr's link value (written to rd) is pc+4 as a byte address --
    # NOT `target` (sqlcpu's `tgt`/this file's TGT, the jump target). The
    # Phase 0 prototype used the same column for both, which is only
    # harmless in a benchmark that never executes its decode data
    # (RESULTS.md §6) -- sqlcpu caught this reviewing PR #42's schema and
    # this PR initially carried the bug forward from fold_predecoded.py.
    # Computed live from the accumulator's own pc, not decoded, since it's
    # simple pc-relative arithmetic with nothing to gain from precomputing.
    # Now that PC is a byte address (see the IDX note above), this is just
    # PC+4 -- no more word/byte conversion needed at this boundary.
    LINK_VALUE = f"toUInt32({PC} + 4)"

    # jalr target: (rs1 + imm) with bit 0 cleared per the RV32I spec --
    # NOT further shifted. Bit 1 (a target that's 2-byte aligned but not
    # 4-byte aligned) is deliberately left intact so the MISALIGNED check
    # below can see it; sqlcpu's execute.py computes this identically.
    JALR_TARGET = f"bitAnd(toUInt32({A} + {IMM}), 4294967294)"

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
        f"{ID}={config.OP_LOAD}, {LOADV},"
        f"{LINK_VALUE})")

    # Fallthrough is PC+4, unclamped -- the real byte address, not rounded
    # into decode-array bounds (IDX's `least` clamp handles array-lookup
    # safety independently; this value is what commits to cpu_state.pc).
    FALLTHROUGH = f"toUInt32({PC}+4)"
    NEXT = ("multiIf("
        f"{ID}=20, if({A} = {B},  {TGT}, {FALLTHROUGH}),"
        f"{ID}=21, if({A} != {B}, {TGT}, {FALLTHROUGH}),"
        f"{ID}=22, if({SA} < {SB},  {TGT}, {FALLTHROUGH}),"
        f"{ID}=23, if({SA} >= {SB}, {TGT}, {FALLTHROUGH}),"
        f"{ID}=24, if({A} < {B},  {TGT}, {FALLTHROUGH}),"
        f"{ID}=25, if({A} >= {B}, {TGT}, {FALLTHROUGH}),"
        f"{ID}=26, {TGT},"
        f"{ID}=27, {JALR_TARGET},"
        f"{FALLTHROUGH})")

    is_load = f"{ID}={config.OP_LOAD}"
    is_store = f"{ID}={config.OP_STORE}"
    is_mem = f"({is_load} OR {is_store})"
    is_ecall = f"{ID}={config.OP_ECALL}"
    is_ebreak = f"{ID}={config.OP_EBREAK}"
    is_csr = f"{ID}={config.OP_CSR}"
    is_illegal = f"{ID}={config.OP_ILLEGAL}"

    # SPEC §1 / issue #37 (ruled): a misaligned jump/branch target halts
    # eagerly AT THE TRANSFERRING INSTRUCTION -- not deferred to whatever
    # would have fetched it next -- with neither pc nor rd updated. jal
    # (26) and jalr (27) always transfer; branches (20-25) only when taken,
    # so "would this instruction actually jump" has to be re-derived here
    # (same conditions NEXT already computes, just as a boolean rather than
    # a pc value) -- unreachable with a well-formed RV32IM binary (no
    # compressed extension means every real target is 4-byte aligned, and
    # jalr clears bit 0), which is exactly why this needs to be pinned by
    # agreement and a test rather than left to "it'll never happen".
    would_jump = ("multiIf("
        f"{ID}=20, {A} = {B},"
        f"{ID}=21, {A} != {B},"
        f"{ID}=22, {SA} < {SB},"
        f"{ID}=23, {SA} >= {SB},"
        f"{ID}=24, {A} < {B},"
        f"{ID}=25, {A} >= {B},"
        f"{ID}=26, true,"
        f"{ID}=27, true,"
        "false)")
    jump_target_if_taken = f"if({ID}=27, {JALR_TARGET}, {TGT})"
    is_jump_op = f"({ID} >= 20 AND {ID} <= 27)"
    jump_misaligned = (f"({is_jump_op} AND ({would_jump})"
                        f" AND bitAnd({jump_target_if_taken}, 3) != 0)")

    # One multiIf computing the halt reason directly (0 = does not halt),
    # so bad_addr_cond/misaligned_cond/self-modify each appear once here
    # instead of being re-derived separately for "does this halt", "what's
    # the reason", and "what's the extra halt-record field" (an earlier
    # version of this function did exactly that -- three-to-four-fold
    # duplication of the same checks, each substituted again at every one
    # of the accumulator's 11 fields; measurably not free under Phase 0's
    # node-count cost model). Every other place below just compares this
    # one value against small integer constants.
    HALT_CODE = ("multiIf("
        f"{is_illegal}, {config.HALT_ILLEGAL_INSN},"
        f"{is_ecall}, {config.HALT_ECALL},"
        f"{is_ebreak}, {config.HALT_EBREAK},"
        f"{is_csr}, {config.HALT_CSR},"
        f"{jump_misaligned}, {config.HALT_MISALIGNED},"
        f"{is_mem} AND {bad_addr_cond}, {config.HALT_BAD_ADDR},"
        f"{is_mem} AND NOT {bad_addr_cond} AND {misaligned_cond}, {config.HALT_MISALIGNED},"
        f"{is_store} AND NOT {bad_addr_cond} AND NOT {misaligned_cond}"
        f" AND {WA} >= {text_start_widx} AND {WA} < {text_end_widx}, {config.HALT_SELF_MODIFY},"
        f"{config.HALT_NONE})")

    active = f"(NOT {STOPPED})"
    step_halts_now = f"({active} AND ({HALT_CODE}) != 0)"
    step_retires = f"({active} AND ({HALT_CODE}) = 0)"

    # halt_extra: the raw instruction word for ILLEGAL_INSN, the faulting
    # target for a misaligned jump/branch, or the faulting address for a
    # data-side BAD_ADDR/MISALIGNED/SELF_MODIFY. `jump_misaligned` must be
    # checked before the generic is_mem-driven ADDR fallback since a jump's
    # "address" (jump_target_if_taken) and a load/store's (ADDR) are
    # different expressions computed from different fields.
    halt_extra_calc = (f"if(({HALT_CODE}) = {config.HALT_ILLEGAL_INSN}, {RAW},"
                        f" if({jump_misaligned}, {jump_target_if_taken},"
                        f" if(({HALT_CODE}) IN ({config.HALT_BAD_ADDR}, {config.HALT_MISALIGNED}, {config.HALT_SELF_MODIFY}), {ADDR}, toUInt32(0))))")

    is_retiring_store = f"({step_retires} AND {is_store})"
    new_wl_len_after_store = f"(toUInt32(length(acc.3.1)) + 1)"
    hits_hwm = f"({is_retiring_store} AND {new_wl_len_after_store} >= {hwm})"

    new_wl = (f"if({is_retiring_store},"
              f" tuple(arrayPushBack(acc.3.1, {WA}), arrayPushBack(acc.3.2, {SVAL}),"
              f" arrayPushBack(acc.3.3, toUInt64(acc.5) + 1)),"
              f" acc.3)")
    new_control = (f"multiIf({step_halts_now},"
                   f" tuple(toUInt8(1), toUInt8(1), toUInt8({HALT_CODE}), {PC}, {halt_extra_calc}),"
                   f" {hits_hwm}, tuple(toUInt8(1), acc.4.2, acc.4.3, acc.4.4, acc.4.5),"
                   f" acc.4)")

    step_tuple = ("tuple("
        f"if({step_retires}, {NEXT}, {PC}),"
        f"if({step_retires} AND {RD} != 0,"
        # 31-element, 1-indexed (regs[1]=x1..regs[31]=x31, sqlcpu's schema.sql):
        # register RD (guaranteed != 0 here) lives at array position RD
        # directly, not RD+1 -- the old 32-element/x0-at-slot-1 scheme this
        # replaced needed the +1.
        f" arrayConcat(arraySlice(acc.2, 1, {RD} - 1), [toUInt32({RESULT})], arraySlice(acc.2, {RD} + 1)),"
        f" acc.2),"
        f"{new_wl},"
        f"{new_control},"
        f"if({step_retires}, toUInt32(acc.5 + 1), acc.5))")
    return step_tuple


# Column names match sqlcpu/schema.sql (PR #42/#46/#49) exactly -- id/tgt/
# mk/sg, not the SPEC §5 prose's op_id/target/width_mask/sign_bit -- per
# sqlcpu's request to reconcile this in the same pass as the PC fix, so
# switching from executor/schema_fixture.sql to the real decoded table
# needs no changes here.
# One combined groupArray(tuple(...)) per table, not one groupArray per
# column. sqlcpu found (PR #67) that ClickHouse 26.3's
# `optimize_read_in_order` (on by default) can stream a column straight
# from its physically-sorted storage into groupArray without honoring the
# subquery's ORDER BY -- silently misaligning that one column against
# word_addr while sibling columns, captured the identical way in the same
# query, stay correct. `SETTINGS optimize_read_in_order = 0` fixes it but
# is a setting a future query can omit, not a structural fix. Capturing
# every column of a table in ONE tuple, in ONE groupArray call, can't
# misalign columns against each other, because they're read and packed
# together -- there's no second independent read path for the optimizer to
# diverge onto. Tried to reproduce this directly against fold.py's own
# tables (single-part and multi-part) and couldn't, but the fix is free
# (verified: no correctness or throughput difference either way) and
# removes a setting-dependent landmine, so it's applied regardless of
# whether it's currently biting this table's specific size/shape.
def decode_with(db=DB):
    """`db` is overridable (default: DB, the production database) so a
    benchmark can point this at its own isolated database -- see #26/the
    batch-overhead investigation on PR #78's thread: a throughput benchmark
    that mutates the real `clickdoom_executor` tables collides silently with
    anything else touching them concurrently (sqlcpu's riscv-tests harness,
    another benchmark run), corrupting the measurement with no error on
    either side. The SQL text this produces is otherwise byte-identical
    regardless of `db` -- only the qualified table names change, never the
    schema, engine, or query shape, so this costs nothing to the numbers a
    benchmark against the real database would produce."""
    return f"""
  (SELECT groupArray(tuple(value))
     FROM (SELECT value, word_addr FROM {db}.ram FINAL ORDER BY word_addr)) AS RAMT,
  (SELECT groupArray(tuple(id, rd, rs1, rs2, imm, tgt, mk, sg, raw))
     FROM (SELECT id, rd, rs1, rs2, imm, tgt, mk, sg, raw, word_addr
           FROM {db}.decoded ORDER BY word_addr)) AS DEC"""

INIT_ACC = ("tuple(toUInt32({pc0}), {regs0},"
            " tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()),"
            " tuple(toUInt8(0), toUInt8(0), toUInt8(0), toUInt32(0), toUInt32(0)),"
            " toUInt32(0))")


def select_only(K, text_start_widx, text_end_widx, decn, ram_words, hwm, pc0=None, regs0=None, db=DB):
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm)
    # 31 elements (x1..x31), no x0 slot -- matches sqlcpu's schema.sql (PR #42).
    regs0_sql = ("[" + ",".join(str(x) for x in regs0) + "]") if regs0 else \
        "arrayResize(emptyArrayUInt32(), 31, toUInt32(0))"
    # pc0 default is RAM_BASE (SPEC §1's reset value 0x8000_0000), not 0 --
    # pc is a byte address now, not a word index relative to the text window.
    pc0 = config.RAM_BASE if pc0 is None else pc0
    init = INIT_ACC.format(pc0=pc0, regs0=regs0_sql)
    return f"""WITH{decode_with(db)}
SELECT r.1 AS pc, r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason, r.4.4 AS halt_pc,
       r.4.5 AS halt_extra, r.5 AS retired
FROM (SELECT arrayFold((acc, i) -> {step}, range({K}), {init}) AS r)
SETTINGS max_threads = 1"""


def batch(K, text_start_widx, text_end_widx, decn, ram_words, hwm, out_table="batch_out", db=DB):
    """One full batch: reload prior state, fold up to K instructions, stage
    the result (including the halt record and per-store icounts) into
    `out_table`. Flushing wl_* into `ram` and cpu_state into the SPEC §5
    table is a separate statement -- deliberately out of scope here; #25
    owns the atomic-commit shape (batch_commit, pending ratification)."""
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm)
    init = INIT_ACC.format(pc0="assumeNotNull(PREV.2)", regs0="CAST(PREV.3, 'Array(UInt32)')")
    # The fold goes in a subquery and the 13 columns are projected off `r`
    # outside it -- NOT `(arrayFold(...) AS r).1, r.2, ...` in one SELECT list.
    # ClickHouse does not common-subexpression the alias in that position: each
    # of the 13 references re-runs the entire fold. Measured at K=1 on the real
    # ROM, where the fold itself is nearly free and the number is almost pure
    # overhead: 16.95s inline vs 1.70s wrapped, a 10x difference in fixed
    # per-batch cost, with identical results.
    #
    # select_only() already had this shape, which is why the two disagreed so
    # sharply on fixed cost (1.7s vs 17s) and why #80's "batch overhead" looked
    # like an unavoidable property of INSERT ... SELECT. It was not; it was this.
    return f"""INSERT INTO {db}.{out_table}
WITH{decode_with(db)},
  (SELECT tuple(batch_id, pc, regs, icount)
     FROM {db}.state ORDER BY batch_id DESC LIMIT 1) AS PREV
SELECT toUInt64(assumeNotNull(PREV.1) + 1) AS batch_id,
       toUInt64(assumeNotNull(PREV.4)) AS icount_before,
       r.1 AS pc, r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason,
       r.4.4 AS halt_pc, r.4.5 AS halt_extra, r.5 AS retired
FROM (SELECT arrayFold((acc, i) -> {step}, range({K}), {init}) AS r)
SETTINGS max_threads = 1"""


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("K", type=int)
    p.add_argument("--hwm", type=int, default=config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT)
    p.add_argument("--text-words", type=int, default=config.TEXT_WORDS_DEFAULT)
    p.add_argument("--ram-words", type=int, default=config.RAM_WORDS_DEFAULT)
    p.add_argument("--e2e", action="store_true")
    p.add_argument("--db", default=DB,
                   help="database to generate SQL against (default: %(default)s). "
                        "Override for a benchmark run isolated onto its own database.")
    args = p.parse_args()

    if args.e2e:
        print(batch(args.K, 0, args.text_words, args.text_words, args.ram_words, args.hwm, db=args.db))
    else:
        print(select_only(args.K, 0, args.text_words, args.text_words, args.ram_words, args.hwm, db=args.db))
