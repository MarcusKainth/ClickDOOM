#!/usr/bin/env python3
"""The batch fold statement (#23): SPEC §1 halt semantics, SPEC §6 early
termination (halt / write-log high-water mark -- FRAME_COMMIT hooks in at
#24, once MMIO address routing exists), and the write-log versioning fix
(SPEC §5, flagged on PR #30 and in issue #35): every write-log entry carries
its own retiring instruction's icount, not the batch's final icount.

Starting point: executor/bench/phase0/fold_predecoded.py (ADR-0002). Reused
verbatim: the collapsed op_id space 0-27, the accumulator's RAM/write-log
addressing idiom, the register-write guard on rd=0. New in this file:

  * op_id 28-31 (ecall/ebreak/csr/illegal) -- SPEC §1 fatal-halt decode arms.
    Not yet agreed with sqlcpu; flagged in the PR for coordination.
  * Address-bounds and load/store-alignment checks (SPEC §1, §2) ahead of
    every memory access, using the "mask to a safe index, check the real
    bound separately" idiom so an out-of-range address can never make
    arrayElement throw, whether or not the access is what triggers the halt.
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

    Accumulator (5-tuple): pcidx, regs[32], wl, control, retired, where
    wl = tuple(addr[], val[], icount[]) and
    control = tuple(stopped, halted, halt_reason, halt_pc, halt_extra).

    Deliberately NOT an 11-flat-field tuple (an earlier version of this
    function was exactly that): every accumulator field needs its own
    `if(step_retires/step_halts_now, ..., acc.N)` guard, and Phase 0's
    cost model charges per node regardless of which branch is "taken" --
    so 11 fields means the halt/retire condition gets evaluated ~10 times
    per step. Packing the write-log's three arrays and the halt record's
    five scalars into one tuple field each cuts that to ~2 evaluations,
    measured to matter (see the PR's before/after numbers).
    """
    decm = decn - 1
    assert (decn & decm) == 0, "decode table length must be a power of two"

    PC = "acc.1"
    IDX = "(acc.1 + 1)"
    STOPPED = "acc.4.1"

    ID = f"DID[{IDX}]"
    RD = f"DRD[{IDX}]"
    IMM = f"DIM[{IDX}]"
    TGT = f"DTG[{IDX}]"
    DMKv = f"DMK[{IDX}]"
    DSGv = f"DSG[{IDX}]"
    RAW = f"DRW[{IDX}]"

    A = f"acc.2[DR1[{IDX}] + 1]"
    B = f"toUInt32(acc.2[DR2[{IDX}] + 1] + {IMM})"
    # Raw rs2 (no +imm). B doubles as the ALU/branch second operand (ADR-0002's
    # I-type/R-type collapse relies on +imm there), but a store's *value* is
    # regs[rs2] alone -- imm is the address offset, already spent computing
    # ADDR from A, and must not also land in the stored value. Using B for
    # SVAL here was a latent bug inherited from fold_predecoded.py, caught by
    # this PR's store/load round-trip test.
    RS2V = f"acc.2[DR2[{IDX}] + 1]"
    SA = f"toInt32({A})"
    SB = f"toInt32({B})"

    ADDR, ADDR64, bad_addr_cond, misaligned_cond, WA = _addr_and_align(
        A, IMM, DMKv, ram_base, ram_words)
    SH = f"(8 * bitAnd({ADDR}, 3))"

    # Write-log first (reverse order, last writer wins), then RAM.
    LW = (f"if(arrayLastIndex(z -> z = {WA}, acc.3.1) > 0,"
          f" acc.3.2[arrayLastIndex(z -> z = {WA}, acc.3.1)], RAM[{WA} + 1])")

    LOADV = (f"toUInt32(bitAnd(bitShiftRight({LW}, {SH}), {DMKv})"
             f" - if(bitAnd(bitAnd(bitShiftRight({LW}, {SH}), {DMKv}), {DSGv}) != 0,"
             f" toUInt64({DMKv}) + 1, 0))")

    SVAL = (f"toUInt32(bitOr(bitAnd({LW}, bitXor(4294967295, toUInt32(bitShiftLeft({DMKv}, {SH})))),"
            f" toUInt32(bitShiftLeft(bitAnd({RS2V}, {DMKv}), {SH}))))")

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
        f"{TGT})")

    NEXT = ("multiIf("
        f"{ID}=20, if({A} = {B},  {TGT}, toUInt32({PC}+1)),"
        f"{ID}=21, if({A} != {B}, {TGT}, toUInt32({PC}+1)),"
        f"{ID}=22, if({SA} < {SB},  {TGT}, toUInt32({PC}+1)),"
        f"{ID}=23, if({SA} >= {SB}, {TGT}, toUInt32({PC}+1)),"
        f"{ID}=24, if({A} < {B},  {TGT}, toUInt32({PC}+1)),"
        f"{ID}=25, if({A} >= {B}, {TGT}, toUInt32({PC}+1)),"
        f"{ID}=26, {TGT},"
        f"{ID}=27, toUInt32(bitAnd(bitShiftRight(toUInt32({A} + {IMM}), 2), {decm})),"
        f"toUInt32(bitAnd({PC}+1, {decm})))")

    is_load = f"{ID}={config.OP_LOAD}"
    is_store = f"{ID}={config.OP_STORE}"
    is_mem = f"({is_load} OR {is_store})"
    is_ecall = f"{ID}={config.OP_ECALL}"
    is_ebreak = f"{ID}={config.OP_EBREAK}"
    is_csr = f"{ID}={config.OP_CSR}"
    is_illegal = f"{ID}={config.OP_ILLEGAL}"

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
        f"{is_mem} AND {bad_addr_cond}, {config.HALT_BAD_ADDR},"
        f"{is_mem} AND NOT {bad_addr_cond} AND {misaligned_cond}, {config.HALT_MISALIGNED},"
        f"{is_store} AND NOT {bad_addr_cond} AND NOT {misaligned_cond}"
        f" AND {WA} >= {text_start_widx} AND {WA} < {text_end_widx}, {config.HALT_SELF_MODIFY},"
        f"{config.HALT_NONE})")

    active = f"(NOT {STOPPED})"
    step_halts_now = f"({active} AND ({HALT_CODE}) != 0)"
    step_retires = f"({active} AND ({HALT_CODE}) = 0)"

    halt_extra_calc = f"if(({HALT_CODE}) = {config.HALT_ILLEGAL_INSN}, {RAW}," \
                       f" if(({HALT_CODE}) IN ({config.HALT_BAD_ADDR}, {config.HALT_MISALIGNED}, {config.HALT_SELF_MODIFY}), {ADDR}, toUInt32(0)))"

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
        f" arrayConcat(arraySlice(acc.2, 1, {RD}), [toUInt32({RESULT})], arraySlice(acc.2, {RD}+2)),"
        f" acc.2),"
        f"{new_wl},"
        f"{new_control},"
        f"if({step_retires}, toUInt32(acc.5 + 1), acc.5))")
    return step_tuple


DECODE_WITH = f"""
  (SELECT groupArray(value) FROM (SELECT value FROM {DB}.ram FINAL ORDER BY word_addr)) AS RAM,
  (SELECT groupArray(op_id)     FROM (SELECT op_id, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DID,
  (SELECT groupArray(rd)        FROM (SELECT rd, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DRD,
  (SELECT groupArray(rs1)       FROM (SELECT rs1, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DR1,
  (SELECT groupArray(rs2)       FROM (SELECT rs2, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DR2,
  (SELECT groupArray(imm)       FROM (SELECT imm, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DIM,
  (SELECT groupArray(target)    FROM (SELECT target, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DTG,
  (SELECT groupArray(width_mask) FROM (SELECT width_mask, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DMK,
  (SELECT groupArray(sign_bit)  FROM (SELECT sign_bit, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DSG,
  (SELECT groupArray(raw)       FROM (SELECT raw, word_addr FROM {DB}.decoded ORDER BY word_addr)) AS DRW"""

INIT_ACC = ("tuple(toUInt32({pc0}), {regs0},"
            " tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()),"
            " tuple(toUInt8(0), toUInt8(0), toUInt8(0), toUInt32(0), toUInt32(0)),"
            " toUInt32(0))")


def select_only(K, text_start_widx, text_end_widx, decn, ram_words, hwm, pc0=0, regs0=None):
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm)
    regs0_sql = ("[" + ",".join(str(x) for x in regs0) + "]") if regs0 else \
        "arrayResize(emptyArrayUInt32(), 32, toUInt32(0))"
    init = INIT_ACC.format(pc0=pc0, regs0=regs0_sql)
    return f"""WITH{DECODE_WITH}
SELECT r.1 AS pcidx, r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason, r.4.4 AS halt_pc,
       r.4.5 AS halt_extra, r.5 AS retired
FROM (SELECT arrayFold((acc, i) -> {step}, range({K}), {init}) AS r)
SETTINGS max_threads = 1"""


def batch(K, text_start_widx, text_end_widx, decn, ram_words, hwm, out_table="batch_out"):
    """One full batch: reload prior state, fold up to K instructions, stage
    the result (including the halt record and per-store icounts) into
    `out_table`. Flushing wl_* into `ram` and cpu_state into the SPEC §5
    table is a separate statement -- deliberately out of scope here; #25
    owns the atomic-commit shape (batch_commit, pending ratification)."""
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm)
    init = INIT_ACC.format(pc0="assumeNotNull(PREV.2)", regs0="CAST(PREV.3, 'Array(UInt32)')")
    return f"""INSERT INTO {DB}.{out_table}
WITH{DECODE_WITH},
  (SELECT tuple(batch_id, pcidx, regs, icount)
     FROM {DB}.state ORDER BY batch_id DESC LIMIT 1) AS PREV
SELECT toUInt64(assumeNotNull(PREV.1) + 1) AS batch_id,
       toUInt64(assumeNotNull(PREV.4)) AS icount_before,
       (arrayFold((acc, i) -> {step}, range({K}), {init}) AS r).1 AS pcidx,
       r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason,
       r.4.4 AS halt_pc, r.4.5 AS halt_extra, r.5 AS retired
SETTINGS max_threads = 1"""


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("K", type=int)
    p.add_argument("--hwm", type=int, default=config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT)
    p.add_argument("--text-words", type=int, default=config.TEXT_WORDS_DEFAULT)
    p.add_argument("--ram-words", type=int, default=config.RAM_WORDS_DEFAULT)
    p.add_argument("--e2e", action="store_true")
    args = p.parse_args()

    if args.e2e:
        print(batch(args.K, 0, args.text_words, args.text_words, args.ram_words, args.hwm))
    else:
        print(select_only(args.K, 0, args.text_words, args.text_words, args.ram_words, args.hwm))
