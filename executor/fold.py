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


def _fb_pal_wa_provably_outside_text(ram_base, ram_words, text_end_widx):
    """#130: proves (not assumes) that `_addr_and_align`'s WA underflow for
    a FRAMEBUFFER/PALETTE address (both below `ram_base`) can never clamp
    into THIS build's actual text window, so HALT_CODE's SELF_MODIFY arm
    can skip its runtime `NOT is_fb_or_pal_store` guard -- see that arm's
    comment for why skipping it is worth doing when provable (the runtime
    version costs roughly a third of #130's total contribution to
    generated SQL size under this file's per-reference-count cost model).

    `wa_safe` computes `least(toUInt32(toUInt64(ADDR) - ram_base) >> 2,
    ram_words - 1)`. For ADDR < ram_base (true of both regions, since
    SPEC §2 places them below RAM), the UInt64 subtraction underflows and
    `toUInt32(...)` truncation of that wrapped value equals `2**32 -
    (ram_base - ADDR)` as long as `ram_base - ADDR <= 2**32` (always true
    for real UInt32 addresses). Below, `shifted` computes that for the
    worst case (smallest ADDR in the region, which underflows the most).
    If `shifted >= ram_words - 1`, the clamp always saturates at the
    `ram_words - 1` ceiling for every address either region spans -- but
    that ceiling is only outside the text window if `ram_words - 1` is
    itself >= `text_end_widx` (text windows start at 0 in every caller
    this file has). A first version of this function checked only the
    saturation half and asserted the ceiling was "guaranteed outside any
    real text window" without checking against `text_end_widx` at all --
    true for SPEC's real RAM/ROM sizing (RAM is ~6.29M words, the ROM's
    text region ~1.2M), FALSE for a small test fixture that intentionally
    sets `ram_words == decn == text_end_widx` to stress exactly this edge
    (test_framebuffer_store_does_not_trigger_self_modify) -- caught by
    that test actually failing (halt_reason=SELF_MODIFY on a clean
    FRAMEBUFFER store), not caught by this function, which is why it now
    checks the real condition and returns a bool instead of asserting
    unconditionally: the proof holds for production sizing and doesn't
    for that fixture's, and both need to keep working, so `build_step`
    uses the answer to choose which SQL to generate rather than this
    function picking one universal answer for every caller."""
    for base, size in ((config.FRAMEBUFFER_BASE, config.FRAMEBUFFER_SIZE),
                        (config.PALETTE_BASE, config.PALETTE_SIZE)):
        assert base < ram_base, (
            f"#130's SELF_MODIFY-arm optimization assumes region base {base:#x} "
            f"is below ram_base {ram_base:#x} -- it underflows WA on purpose; "
            f"if this ever isn't true, the runtime guard needs to come back.")
        worst_case_addr = base  # smallest ADDR in the region -> largest (ram_base - ADDR)
        shifted = ((2**32 - (ram_base - worst_case_addr)) & 0xFFFFFFFF) >> 2
        if shifted < ram_words - 1 or ram_words - 1 < text_end_widx:
            return False
    return True


def build_step(K, text_start_widx, text_end_widx, decn, ram_words,
               ram_base=config.RAM_BASE, hwm=config.WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
               ipms=config.IPMS_DEFAULT):
    """Returns the arrayFold lambda body: `(acc, i) -> tuple(...)`.

    Accumulator (7-tuple): pc, regs[31], wl, control, icount, mmio, fbpal_wl,
    where pc is a byte address (matching cpu_state.pc, sqlcpu's schema.sql),
    regs is x1..x31 (no x0 slot), wl = tuple(addr[], val[], icount[])
    (RAM's write-log), control = tuple(stopped, halted, halt_reason,
    halt_pc, halt_extra), mmio = tuple(console_bytes[], keyq_pos,
    tuple(frame_no, frame_committed)), and fbpal_wl (#130) = tuple(
    fb_addr[], fb_val[], fb_icount[], pal_addr[], pal_val[], pal_icount[])
    -- FRAMEBUFFER/PALETTE's own write-log lanes, packed into one field for
    the same field-count reason RAM's three arrays share acc.3 rather than
    getting a slot each, and kept SEPARATE from acc.3 because these lanes
    are never scanned (no arrayLastIndex against them, ever -- nothing
    reads FRAMEBUFFER/PALETTE, so there's nothing to search for) while
    RAM's write-log is searched on every load.

    acc.5 is the ABSOLUTE icount (UInt64), not a per-batch retired count --
    seeded from the batch's starting icount in INIT_ACC, incremented by 1
    per retiring step. This is deliberate, not incidental: an earlier
    version threaded a separate `icount_base` string into this function and
    baked it into TICKS_MS/wl_icount's SQL text on every call. That value
    changes every batch (`assumeNotNull(PREV.4)` in batch()), and a
    per-batch-varying literal *inside the lambda body* is part of
    ClickHouse's compiled-expression cache key (`compile_expressions`,
    default on, warms after 3 executions) -- so the lambda text differed
    every batch and the JIT never compiled, ever, silently. Discovered by
    an external reviewer's hypothesis, corroborated independently via
    `system.query_log`'s `CompileFunction`/`CompileExpressionsMicroseconds`
    ProfileEvents reading zero on every batch() run. Seeding acc.5 from
    PREV.4 instead keeps the lambda's own text byte-identical batch to
    batch (only `range(K)`'s K and `{ipms}`/`{hwm}` vary, and those are
    already per-run constants, not per-batch) -- the accumulator's INITIAL
    VALUE is a runtime argument to arrayFold, not part of the compiled
    lambda, so a varying value there costs nothing.

    Real-fold measurement, not just the isolated microbenchmark that found
    the bug: the reviewer's representative variant measured ~5.6x, but
    that variant is small and simple next to this fold's full ~90k-node
    expression (MMIO, halt semantics, decode dispatch, the write-log). On
    six chained real-ROM batches (K=49,152, `PINNED_HASH e74cf575...`),
    `system.query_log` confirms compilation triggers on the 4th batch
    (`CompileFunction=3`, `CompileExpressionsMicroseconds=12652`) and stays
    compiled after -- but the speedup on THIS query is ~1.23x (46.48s avg
    over batches 1-3, 37.77s avg over batches 4-6), not 5.6x. Real,
    verified by the compile counters actually firing (not just a faster
    wall-clock, which could mean something else changed) -- just smaller
    at this expression's size than the microbenchmark that found the bug.
    Reported as measured, not extrapolated from the smaller case.

    The outer SELECT (select_only()/batch(),
    outside the lambda entirely) derives "retired this batch" as
    `final_icount - starting_icount` where a varying value is harmless.

    Deliberately NOT an 11-flat-field tuple (an earlier version of this
    function was exactly that): every accumulator field needs its own
    `if(step_retires/step_halts_now, ..., acc.N)` guard, and Phase 0's
    cost model charges per node regardless of which branch is "taken" --
    so 11 fields means the halt/retire condition gets evaluated ~10 times
    per step. Packing the write-log's three arrays and the halt record's
    five scalars into one tuple field each cuts that to ~2 evaluations,
    measured to matter (see the PR's before/after numbers).
    """
    # #146: text_start_widx/text_end_widx are compared directly against WA
    # below, and WA is RAM_BASE-relative, clamped to [0, ram_words-1] by
    # construction (_addr_and_align's wa_safe) -- never an absolute word
    # address. This is the third time an absolute/relative word-address
    # mix-up has slipped past every existing test silently in this project
    # (#81's RAMT indexing, the write-log's wl_addr-into-ram.word_addr
    # flush, and #146 itself, independently, in two different callers'
    # own gate/runner scripts) -- nothing in this function's signature
    # enforces the unit otherwise, so a loud failure here beats a fourth
    # silent one. `text_end_widx <= ram_words` alone would have caught
    # #146's actual numbers (536,969,736 > 6,291,456); the rest of the
    # range makes the invariant precise, not just sufficient for that one
    # bug. Checked first, before anything below derives from these values
    # (including _fb_pal_wa_provably_outside_text's own text_end_widx
    # argument) -- a precondition on the whole function, not just on WA's
    # own comparison further down.
    assert 0 <= text_start_widx <= text_end_widx <= ram_words, (
        f"text_start_widx={text_start_widx}/text_end_widx={text_end_widx} must be "
        f"RAM_BASE-relative word indices with 0 <= text_start_widx <= text_end_widx "
        f"<= ram_words={ram_words} -- got a value outside that range, which is what "
        f"an absolute word address looks like here (#146)"
    )

    fb_pal_wa_outside_text = _fb_pal_wa_provably_outside_text(
        ram_base, ram_words, text_end_widx)

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
    # --- SPEC §3 MMIO ------------------------------------------------------
    # The MMIO window is a *valid* address range, so `bad_addr_cond` (which
    # means "outside RAM") is no longer the whole bad-address test: an access
    # is bad only if it is outside RAM AND outside MMIO.
    # bitAnd against the window mask, not `>= base AND < end`: one reference to
    # ADDR instead of two. With no let-binding available inside the fold's
    # lambda, every textual reference to ADDR re-expands its whole subtree
    # (two DEC lookups and a register read), so reference *count* is the thing
    # that costs -- see the node-budget note at the top of the MMIO section.
    assert config.MMIO_SIZE == 4096, "window mask below assumes a 4 KiB window"
    is_mmio = f"(bitAnd({ADDR}, {0xFFFFFFFF ^ (config.MMIO_SIZE - 1)}) = {config.MMIO_BASE})"

    # --- SPEC §2 FRAMEBUFFER/PALETTE (#130) ---------------------------------
    # Write-only from this side: the routing exemption below is gated on
    # `is_store` (inlined as `{ID}={config.OP_STORE}`, matching
    # `is_mmio_store`'s own style below rather than the `is_store` variable
    # defined later in this function -- kept local rather than reordering
    # existing definitions), so a LOAD from either region is untouched by
    # this block and falls through to the existing bad_addr_cond exactly as
    # before -- BAD_ADDR, by construction, not by a new check. That is
    # deliberate: SPEC §2 doesn't yet say these regions are readable (issue
    # #134, pending ratification), and not routing loads at all is cheaper
    # than routing them to always-fail.
    #
    # Not a bitAnd window mask like MMIO's, since 64,000 and 768 aren't
    # power-of-two sized -- same reasoning as `_addr_and_align`'s RAM bound,
    # a real range compare instead.
    # ONE reference to ADDR each, not two: `toUInt32(ADDR - BASE) < SIZE`
    # covers both bounds in one unsigned-subtraction-and-compare, same idiom
    # `_addr_and_align`'s own bad_addr_cond uses for RAM (`ADDR64 - RAM_BASE`,
    # implicitly, via ram_end) -- an address below BASE underflows to a huge
    # UInt32 that's >= SIZE, an address at or past BASE+SIZE is directly >=
    # SIZE, and only [BASE, BASE+SIZE) computes something < SIZE. Load-
    # bearing here specifically: this file has no let-binding inside the
    # fold lambda, so every *textual* reference to ADDR re-expands its whole
    # subtree (DEC lookups, register reads) -- measured going from 2 refs
    # each to 1 nearly halved this feature's contribution to generated SQL
    # size (`is_fb_or_pal_store` below is itself referenced 4 more times
    # inside HALT_CODE, which is referenced 5 more times downstream of
    # that -- reference count compounds multiplicatively here, not
    # additively, which is why halving the leaf cost matters this much).
    is_fb = f"(toUInt32(toUInt64({ADDR}) - {config.FRAMEBUFFER_BASE}) < {config.FRAMEBUFFER_SIZE})"
    is_pal = f"(toUInt32(toUInt64({ADDR}) - {config.PALETTE_BASE}) < {config.PALETTE_SIZE})"
    is_fb_or_pal_store = f"({ID}={config.OP_STORE} AND ({is_fb} OR {is_pal}))"
    bad_addr_cond = f"({bad_addr_cond} AND NOT {is_mmio} AND NOT {is_fb_or_pal_store})"

    # Byte offset within the window. Compared against the five register
    # offsets exactly rather than masked -- a mask would alias every 32 bytes
    # of the 4 KiB window onto the registers, turning a stray access into a
    # silent EXIT or a phantom console byte.
    def mmio_is(reg):
        """`ADDR = <absolute register address>` rather than `MOFF = <offset>`:
        same discrimination, one node fewer per site, and no separate MOFF
        subtree to re-expand."""
        return f"({ADDR} = {config.MMIO_BASE + reg})"

    # Non-register offsets, and non-word widths, read 0 and ignore writes.
    # refemu backs the whole 4 KiB with plain byte storage (mmio.py:79), so
    # this differs from the oracle for a program that writes then reads back
    # non-register MMIO. DOOM does not, and reproducing a byte-addressable
    # scratch region here would cost nodes on every step to serve nothing --
    # but the difference is real and is filed rather than left implicit.
    is_mmio_load = f"({is_mmio} AND {ID} = {config.OP_LOAD})"
    is_mmio_store = f"({is_mmio} AND {ID} = {config.OP_STORE})"

    # TICKS_MS: elastic time, SPEC §3.1. instructions_retired / IPMS. acc.5
    # is now the ABSOLUTE icount (see build_step()'s docstring on why it's
    # not `icount_base + acc.5` anymore -- that form baked a per-batch-
    # varying literal into this exact line and silently defeated ClickHouse's
    # expression JIT). Never wall clock -- SPEC §8.1, and scripts/check_purity.sh
    # greps for it.
    TICKS_MS = f"toUInt32(intDiv(acc.5, {ipms}))"

    # KEYQ: SPEC §3.2. acc.6.2 is how many events this batch has already
    # popped; KEYQT is the queue captured in event_seq order. An empty-queue
    # read returns 0 and pops nothing, so the position advance is guarded by
    # the same bounds test that selects the value.
    KEYQ_HAS = f"(acc.6.2 < toUInt32(length(KEYQT)))"
    KEYQ_VAL = f"if({KEYQ_HAS}, toUInt32(KEYQT[toUInt32(acc.6.2) + 1].1), toUInt32(0))"

    MMIO_READ = (f"multiIf({mmio_is(config.MMIO_TICKS_MS)}, {TICKS_MS},"
                 f" {mmio_is(config.MMIO_KEYQ)}, {KEYQ_VAL},"
                 f" toUInt32(0))")

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

    # A load from the MMIO window takes its value from the register file
    # above, not from RAM/write-log.
    LOADV = f"if({is_mmio}, {MMIO_READ}, {LOADV})"

    # div/rem (id=14/16) widen to Int64 before intDiv/modulo -- #99, P0.
    # Int32 intDiv/modulo throws a hard ILLEGAL_DIVISION on the one
    # representable-but-overflowing case, dividend=INT_MIN (-2^31) and
    # divisor=-1: the mathematical quotient is +2^31, which does not fit
    # back in Int32. ClickHouse traps instead of wrapping. RISC-V's M
    # extension does not trap here -- DIV specifies exactly this case
    # wraps to -2^31 (the same bit pattern, reinterpreted), REM specifies
    # 0 -- and sqlcpu/execute.py already implements that; this matches it
    # rather than re-deriving it. In Int64, -2^31 / -1 = +2^31 is
    # representable with room to spare, so `toUInt32()` truncation at the
    # end produces the spec'd wrapped bit pattern with NO extra branch --
    # not a second guard alongside the existing `if({SB}=0, ...)` zero
    # check, just widening the two operands already being divided. This
    # arm runs unconditionally on every step regardless of the step's real
    # op_id (multiIf/if never short-circuit inside arrayFold, ADR-0002),
    # so a guard here would cost every instruction; the wider cast does
    # not add a node, it changes two already-present toInt64 casts' target
    # width for free.
    # DIVU/REMU (id=15/17) need no equivalent: they operate on unsigned A/B
    # throughout (no signed-overflow representation to escape in the first
    # place), so they're untouched.
    SA64 = f"toInt64({SA})"
    SB64 = f"toInt64({SB})"

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
        f"{ID}=14, if({SB}=0, 4294967295, toUInt32(intDiv({SA64}, {SB64}))),"
        f"{ID}=15, if({B}=0, 4294967295, toUInt32(intDiv({A}, {B}))),"
        f"{ID}=16, if({SB}=0, {A}, toUInt32(modulo({SA64}, {SB64}))),"
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
        # #130: a store to FRAMEBUFFER/PALETTE narrower than a full word has
        # no correct answer to write -- these lanes are never read (that's
        # the whole point of keeping them unscanned), so there is no
        # previous word to read-modify-write a byte/halfword store against,
        # unlike RAM's SVAL. Halting BAD_ADDR rather than silently dropping
        # the store or writing a wrong (zero-padded) word -- team lead's
        # ruling on #130, being folded into #134 as SPEC text rather than
        # left as an implementation-only decision. Checked after
        # misaligned_cond (a genuinely misaligned halfword store should
        # report MISALIGNED, not this) but before SELF_MODIFY, since the
        # two conditions are address-range-disjoint and order between them
        # doesn't otherwise matter.
        #
        # Deliberately NOT re-checking `NOT bad_addr_cond AND NOT
        # misaligned_cond` here, unlike the SELF_MODIFY arm below (which
        # does, matching this file's existing convention) -- multiIf's
        # ordering already establishes both: is_fb_or_pal_store implies
        # is_store implies is_mem, so if execution reaches this arm, the two
        # `is_mem AND ...` arms immediately above did NOT match, which by
        # construction means NOT bad_addr_cond (arm 1 didn't fire) and NOT
        # misaligned_cond (arm 2 didn't fire, and is_mem/NOT bad_addr_cond
        # both hold, so misaligned_cond must be false). Re-deriving either
        # would add two more references to already-multiply-referenced
        # terms (`bad_addr_cond` is itself referenced elsewhere in this
        # same HALT_CODE, and HALT_CODE is referenced several times further
        # downstream in step_retires/step_halts_now/halt_extra_calc) --
        # measured this costing roughly half of this feature's total
        # contribution to generated SQL size before removing it; the
        # multiIf ordering argument is exact, not an approximation, so
        # there's no correctness given up for that saving.
        f"{is_fb_or_pal_store} AND {DMKv} != 4294967295, {config.HALT_BAD_ADDR},"
        # No runtime `AND NOT is_fb_or_pal_store` guard here (an earlier
        # version had one, and it cost roughly a third of this feature's
        # total contribution to generated SQL size -- measured, not
        # guessed, per this file's own cost model where every *textual*
        # reference to an already-multiply-referenced term compounds).
        # `WA` (`(ADDR - RAM_BASE) >> 2`, RAM-relative) underflows for an
        # FRAMEBUFFER/PALETTE address, since both are below RAM_BASE, and
        # `least()`-clamps the wrapped result into [0, RAM_WORDS) -- the
        # concern this guard existed for is that clamp coincidentally
        # landing inside [text_start_widx, text_end_widx) and misfiring
        # SELF_MODIFY on a legitimate framebuffer store. Dropped only when
        # `_fb_pal_wa_provably_outside_text` (checked once at SQL-generation
        # time, above) proves it for THIS build's actual ram_words/
        # text_end_widx -- true for SPEC's real RAM/ROM sizing, false for
        # e.g. a small test fixture whose ram_words/text window happen to
        # coincide. A first version dropped the guard unconditionally on a
        # proof that didn't check text_end_widx at all; caught by
        # test_framebuffer_store_does_not_trigger_self_modify actually
        # failing (SELF_MODIFY on a clean FRAMEBUFFER store), not by the
        # proof itself. Restoring the guard when unprovable keeps every
        # caller correct; production keeps the cheaper SQL because the
        # proof holds there.
        f"{is_store} AND NOT {bad_addr_cond} AND NOT {misaligned_cond}"
        f" AND NOT {is_mmio}"
        + ("" if fb_pal_wa_outside_text else f" AND NOT {is_fb_or_pal_store}")
        + f" AND {WA} >= {text_start_widx} AND {WA} < {text_end_widx}, {config.HALT_SELF_MODIFY},"
        # SPEC §3's EXIT register: the ROM's own clean stop, not a fault.
        # After the misaligned arms so a misaligned write to EXIT faults
        # rather than exiting. NOT {misaligned_cond} guards it directly too,
        # since multiIf arm order is the only thing that would otherwise
        # enforce it and that is fragile to reordering.
        f"{is_mmio_store} AND NOT {misaligned_cond} AND {mmio_is(config.MMIO_EXIT)},"
        f" {config.HALT_EXIT},"
        f"{config.HALT_NONE})")

    active = f"(NOT {STOPPED})"
    # B1 (#151): HALT_CODE is a large multiIf referenced many times
    # downstream (directly here, and via step_retires's own 14 further
    # references across step_tuple) -- each reference re-expanded its
    # entire text (measured: 19 copies of a 14,760-char expression before
    # this change). `HC` is a nested-lambda parameter (see step_tuple's
    # arrayMap wrapper below) that HALT_CODE is bound to exactly once;
    # every reference below is a cheap parameter read, not a re-expansion
    # -- executor/bench/e1_cse's own n40_bound finding, applied to the
    # actual bottleneck instead of a synthetic one.
    HC = "hc"
    step_halts_now = f"({active} AND ({HC}) != 0)"
    step_retires = f"({active} AND ({HC}) = 0)"

    # halt_extra: the raw instruction word for ILLEGAL_INSN, the faulting
    # target for a misaligned jump/branch, or the faulting address for a
    # data-side BAD_ADDR/MISALIGNED/SELF_MODIFY. `jump_misaligned` must be
    # checked before the generic is_mem-driven ADDR fallback since a jump's
    # "address" (jump_target_if_taken) and a load/store's (ADDR) are
    # different expressions computed from different fields.
    # EXIT carries the written value as the exit code, per SPEC §3 -- the same
    # halt_extra slot that carries the raw word for ILLEGAL_INSN and the
    # faulting address for the address faults, rather than a sixth tuple
    # element that every step would have to copy.
    halt_extra_calc = (f"if(({HC}) = {config.HALT_ILLEGAL_INSN}, {RAW},"
                        f" if(({HC}) = {config.HALT_EXIT}, {RS2V},"
                        f" if({jump_misaligned}, {jump_target_if_taken},"
                        f" if(({HC}) IN ({config.HALT_BAD_ADDR}, {config.HALT_MISALIGNED}, {config.HALT_SELF_MODIFY}), {ADDR}, toUInt32(0)))))")

    # NOT is_mmio: an MMIO store's WA is `least((ADDR - RAM_BASE) >> 2, ...)`,
    # a meaningless clamped index. Letting it into the write-log would flush a
    # garbage value over a real RAM word. MMIO store effects land in acc.6.
    # NOT is_fb_or_pal_store: FB/PAL stores get their OWN lane (acc.7,
    # below), never RAM's -- letting one in here would flush a garbage
    # value over a real RAM word, the same reasoning as NOT is_mmio a line
    # above (`WA`'s underflow-clamp for an FB/PAL address is a meaningless
    # index into RAM, same as an MMIO store's).
    is_retiring_store = f"({step_retires} AND {is_store} AND NOT {is_mmio} AND NOT {is_fb_or_pal_store})"
    new_wl_len_after_store = f"(toUInt32(length(acc.3.1)) + 1)"
    hits_hwm = f"({is_retiring_store} AND {new_wl_len_after_store} >= {hwm})"

    # wl_icount is ABSOLUTE by construction, not by convention: acc.5 is
    # already the running absolute icount (build_step()'s docstring), so
    # this store's version is simply "acc.5 after this step retires" --
    # acc.5 + 1, no batch offset to add and no way for a caller to forget
    # to add one, unlike the earlier design where a `icount_base +` term
    # had to be repeated correctly at both this line and every flush site.
    # Matches SPEC's `ram` versioning note ("the individual store's own
    # icount ... not the batch's final icount") -- #101's fix, now
    # structural rather than a convention every reader had to honor
    # separately. (#101 found this was batch-relative in an earlier
    # version, `toUInt64(acc.5) + 1` with a *local* acc.5 that reset to 0
    # each batch; not live corruption at the time, since every flush site
    # added its own `icount_before +` compensation, but a real SPEC
    # violation and a landmine for the next flush site that forgot to.
    # That whole class of bug is gone now that acc.5 itself carries the
    # absolute value -- there is no separate offset left to omit.)
    new_wl = (f"if({is_retiring_store},"
              f" tuple(arrayPushBack(acc.3.1, {WA}), arrayPushBack(acc.3.2, {SVAL}),"
              f" arrayPushBack(acc.3.3, acc.5 + 1)),"
              f" acc.3)")
    new_control = (f"multiIf({step_halts_now},"
                   f" tuple(toUInt8(1), toUInt8(1), toUInt8({HC}), {PC}, {halt_extra_calc}),"
                   f" {hits_hwm}, tuple(toUInt8(1), acc.4.2, acc.4.3, acc.4.4, acc.4.5),"
                   f" acc.4)")

    # --- acc.7: FRAMEBUFFER/PALETTE write-log (#130) ------------------------
    # Never scanned -- no arrayLastIndex against these arrays anywhere in
    # this file, deliberately, since nothing ever reads from either region.
    # Word-only by construction (HALT_CODE's arm above), so the value to
    # write is RS2V directly: no read-modify-write, no LW, no SVAL --
    # there's no previous word to blend a narrower store against, and a
    # word store's mask covers the whole word anyway (RAM's SVAL reduces
    # to the same thing for a word-width store, this just skips computing
    # the reduction). `{is_fb}`/`{is_pal}` were computed once already,
    # above, for the routing exemption -- reused here, not re-derived.
    #
    # No HWM interaction of its own in this PR: a single frame's ~16,000
    # framebuffer stores could grow this lane large within one batch, but
    # since it's never scanned, that growth doesn't inflate load-scan cost
    # the way sharing RAM's write-log would have (the whole reason for a
    # separate lane) -- whether it's still worth its own early-termination
    # trigger is a real open question from the issue, deliberately left for
    # a follow-up once real batch behavior during rendering can be measured
    # rather than guessed at now.
    # `{ID}={config.OP_STORE} AND {is_fb}` directly, NOT
    # `{is_fb_or_pal_store} AND {is_fb}` -- equivalent (is_fb_or_pal_store
    # is exactly `is_store AND (is_fb OR is_pal)`, and ANDing that with
    # is_fb again is redundant once is_fb is already true), but avoids
    # re-expanding is_fb_or_pal_store's larger combined text at two more
    # call sites. Two fewer references to a term that's already referenced
    # several times elsewhere in this function matters under this file's
    # per-textual-reference cost model.
    retiring_fb_store = f"({step_retires} AND {ID}={config.OP_STORE} AND {is_fb})"
    retiring_pal_store = f"({step_retires} AND {ID}={config.OP_STORE} AND {is_pal})"
    fb_wa = f"bitShiftRight(toUInt32(toUInt64({ADDR}) - {config.FRAMEBUFFER_BASE}), 2)"
    pal_wa = f"bitShiftRight(toUInt32(toUInt64({ADDR}) - {config.PALETTE_BASE}), 2)"
    new_fbpal_wl = (
        f"tuple("
        f"if({retiring_fb_store}, arrayPushBack(acc.7.1, {fb_wa}), acc.7.1),"
        f"if({retiring_fb_store}, arrayPushBack(acc.7.2, {RS2V}), acc.7.2),"
        f"if({retiring_fb_store}, arrayPushBack(acc.7.3, acc.5 + 1), acc.7.3),"
        f"if({retiring_pal_store}, arrayPushBack(acc.7.4, {pal_wa}), acc.7.4),"
        f"if({retiring_pal_store}, arrayPushBack(acc.7.5, {RS2V}), acc.7.5),"
        f"if({retiring_pal_store}, arrayPushBack(acc.7.6, acc.5 + 1), acc.7.6))")

    # --- acc.6: MMIO side effects ---------------------------------------
    # console bytes (PUTCHAR), keyq position (KEYQ pops), frame number and
    # frame-committed flag (FRAME_COMMIT). Each guarded so a non-retiring or
    # non-MMIO step carries the previous value through unchanged.
    #
    # A retiring MMIO store, specifically: a step that halts on EXIT must not
    # also push a console byte or commit a frame, and `step_retires` is false
    # for it because HALT_CODE != 0 -- so `retiring_mmio_store` covers both.
    retiring_mmio_store = f"({step_retires} AND {is_mmio_store})"

    new_console = (f"if({retiring_mmio_store} AND {mmio_is(config.MMIO_PUTCHAR)},"
                   f" arrayPushBack(acc.6.1, toUInt8(bitAnd({RS2V}, 255))),"
                   f" acc.6.1)")

    # Guarded increment: a read of an empty queue returns 0 and pops nothing
    # (SPEC §3.2), so the advance is conditioned on the same bounds test that
    # produced the value, not merely on "a KEYQ read happened".
    new_keyq_pos = (f"if({step_retires} AND {is_mmio_load}"
                    f" AND {mmio_is(config.MMIO_KEYQ)} AND {KEYQ_HAS},"
                    f" toUInt32(acc.6.2 + 1), acc.6.2)")

    # frame number and committed-flag share one condition, so they live in a
    # nested tuple and are written by a single `if`. Two separate slots would
    # mean testing FRAME_COMMIT twice, and each test re-expands ADDR.
    new_frame = (f"if({retiring_mmio_store} AND {mmio_is(config.MMIO_FRAME_COMMIT)},"
                 f" tuple({RS2V}, toUInt8(1)), acc.6.3)")

    new_mmio = f"tuple({new_console}, {new_keyq_pos}, {new_frame})"

    step_tuple_inner = ("tuple("
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
        # acc.5 stays UInt64 throughout (seeded from icount0 in INIT_ACC,
        # see build_step()'s docstring) -- no toUInt32() cast here, unlike
        # an earlier version: this is the batch's absolute icount, not a
        # per-batch retired count, and truncating it to UInt32 would
        # silently wrap on a long enough run (demo3's ~2.91B instructions
        # is uncomfortably close to UInt32::MAX already).
        f"if({step_retires}, acc.5 + 1, acc.5),"
        f"{new_mmio},"
        f"{new_fbpal_wl})")
    # Bind HALT_CODE once via a single-element arrayMap, per E1's n40_bound
    # finding -- HC is now a real lambda parameter inside this one call,
    # not a textual macro, so every {HC} reference above costs one name
    # lookup instead of re-expanding HALT_CODE's full multiIf.
    step_tuple = f"arrayMap({HC} -> {step_tuple_inner}, [{HALT_CODE}])[1]"
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
           FROM {db}.decoded ORDER BY word_addr)) AS DEC,
  -- SPEC §3.2: the key queue, in event_seq order, captured the same way as
  -- RAM and the decode table. `consumed` is deliberately NOT filtered here:
  -- whether an event has been consumed is a computed predicate on the
  -- cumulative keyq position (ADR-0003 §5), not a mutated flag, so the fold
  -- must see the whole queue and index into it.
  (SELECT groupArray(tuple(key_event))
     FROM (SELECT key_event, event_seq FROM {db}.input_queue
           ORDER BY event_seq)) AS KEYQT"""

INIT_ACC = ("tuple(toUInt32({pc0}), {regs0},"
            " tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()),"
            " tuple(toUInt8(0), toUInt8(0), toUInt8(0), toUInt32(0), toUInt32(0)),"
            # acc.5: the batch's ABSOLUTE starting icount, not 0 -- this is
            # the JIT fix (build_step()'s docstring): seeding acc.5 here,
            # as arrayFold's initial-value argument, keeps a per-batch-
            # varying value OUT of the lambda's own compiled text, unlike
            # the earlier design that interpolated it into TICKS_MS/
            # wl_icount inside the lambda body on every build_step() call.
            " toUInt64({icount0}),"
            # acc.6, SPEC §3 MMIO: console bytes, keyq position, frame
            # number, frame-committed flag. keyq_pos starts at 0 each batch
            # and is *cumulative across batches* only via batch_commit --
            # see #25; within a batch it indexes KEYQT from {keyq0}.
            " tuple(emptyArrayUInt8(), toUInt32({keyq0}),"
            " tuple(toUInt32(0), toUInt8(0))),"
            # acc.7, SPEC §2 FRAMEBUFFER/PALETTE write-log (#130): six empty
            # arrays, same shape as RAM's acc.3 -- three per lane
            # (addr/val/icount), two lanes, never read back within a batch
            # (no seed value needed beyond empty, since nothing carries over
            # from the previous batch the way keyq_pos/icount/pc do -- these
            # lanes are flushed and forgotten every batch, same as RAM's).
            " tuple(emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64(),"
            " emptyArrayUInt32(), emptyArrayUInt32(), emptyArrayUInt64()))")


def select_only(K, text_start_widx, text_end_widx, decn, ram_words, hwm, pc0=None, regs0=None,
                db=DB, icount0=0, keyq0=0, ipms=config.IPMS_DEFAULT):
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm, ipms=ipms)
    # 31 elements (x1..x31), no x0 slot -- matches sqlcpu's schema.sql (PR #42).
    regs0_sql = ("[" + ",".join(str(x) for x in regs0) + "]") if regs0 else \
        "arrayResize(emptyArrayUInt32(), 31, toUInt32(0))"
    # pc0 default is RAM_BASE (SPEC §1's reset value 0x8000_0000), not 0 --
    # pc is a byte address now, not a word index relative to the text window.
    pc0 = config.RAM_BASE if pc0 is None else pc0
    init = INIT_ACC.format(pc0=pc0, regs0=regs0_sql, keyq0=keyq0, icount0=icount0)
    # r.5 is the fold's final ABSOLUTE icount (acc.5, seeded from icount0);
    # `retired` (this call's own K-or-fewer count) is r.5 - icount0, computed
    # here in the outer SELECT -- outside the lambda, where a per-call-
    # varying icount0 costs nothing (see build_step()'s docstring on the
    # JIT fix this mirrors). Kept as a UInt32 column, same type as before:
    # a single select_only() call is bounded by K, which never approaches
    # UInt32::MAX.
    return f"""WITH{decode_with(db)}
SELECT r.1 AS pc, r.2 AS regs, r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       r.4.1 AS stopped, r.4.2 AS halted, r.4.3 AS halt_reason, r.4.4 AS halt_pc,
       r.4.5 AS halt_extra, toUInt32(r.5 - toUInt64({icount0})) AS retired,
       r.6.1 AS console_bytes, r.6.2 AS keyq_pos, r.6.3.1 AS frame_no, r.6.3.2 AS frame_committed,
       r.7.1 AS fb_wl_addr, r.7.2 AS fb_wl_val, r.7.3 AS fb_wl_icount,
       r.7.4 AS pal_wl_addr, r.7.5 AS pal_wl_val, r.7.6 AS pal_wl_icount
FROM (SELECT arrayFold((acc, i) -> {step}, range({K}), {init}) AS r)
SETTINGS max_threads = 1,
         -- The step expression is generated, not hand-written, and MMIO (#24)
         -- pushed it past ClickHouse's 50,000-node default. This raises a
         -- *limit*, it does not change what is computed: measured AST size is
         -- ~90k nodes and the ceiling is a guard against runaway generated
         -- SQL, which this is not. Node count still costs time under
         -- ADR-0001's model -- that is tracked as throughput, not as a limit.
         max_ast_elements = 500000, max_expanded_ast_elements = 500000,
         -- #130's FRAMEBUFFER/PALETTE lane pushed the generated query text
         -- itself (not just its parsed AST) past ClickHouse's 262,144-byte
         -- default max_query_size -- same "raises a limit, doesn't change
         -- what's computed" reasoning as the AST settings above; the SQL
         -- text is ~440KB, comfortably under this raised ceiling.
         max_query_size = 2000000"""


def _halt_reason_transform(halt_code_expr):
    """`transform(halt_code_expr, [1..8], ['ILLEGAL_INSN', ..., 'EXIT'], '')`,
    generated from config.HALT_REASON_NAMES so the SQL mapping can't drift
    from the Python one -- HALT_NONE (0) isn't in the from-array, so it (and
    anything else unrecognized) falls through to the '' default, matching
    HALT_REASON_NAMES[HALT_NONE] anyway. Evaluated once per batch, outside
    the fold lambda (see batch()'s docstring on why that placement matters,
    #86) -- config.py's own comment already anticipates this exact mapping
    happening "outside the fold, once per batch, not once per step"."""
    codes = [c for c in config.HALT_REASON_NAMES if c != config.HALT_NONE]
    names = [config.HALT_REASON_NAMES[c] for c in codes]
    from_arr = "[" + ",".join(str(c) for c in codes) + "]"
    to_arr = "[" + ",".join(f"'{n}'" for n in names) + "]"
    return f"transform(toUInt8({halt_code_expr}), {from_arr}, {to_arr}, '')"


def batch(K, text_start_widx, text_end_widx, decn, ram_words, hwm, db=DB,
          ipms=config.IPMS_DEFAULT):
    """One full batch: reload prior state (from `batch_commit`, SPEC §5/
    ADR-0003 -- the only table `keyq_pos` lives on), fold up to K
    instructions, and INSERT the SPEC §5-shaped `batch_commit` row directly
    -- this INSERT *is* the batch's single atomic write (ADR-0003 point 1).
    Flushing wl_*/console_bytes into `ram`/`console_out` and deriving
    `cpu_state` are separate, idempotent statements (executor/commit.py,
    #25) -- deliberately not here, since they must be safely re-runnable
    independently of this INSERT ever having happened more than once.

    Column mapping from the fold's accumulator (`r`) to batch_commit's SPEC
    §5 columns, done here in the outer SELECT rather than inside the fold:
    `icount` is `r.5` directly now (acc.5 is already the absolute icount,
    seeded from PREV.4 -- see build_step()'s docstring on why that seeding
    happens in INIT_ACC and not as a term added here) and the halt-reason
    code->string transform is cheap outside the lambda and would cost
    per-step time inside it (ADR-0002's node-count model). `exit_code` is
    EXIT-only, not a general "halt extra" slot: SPEC §1 (#89, ratified this
    session) is explicit that "a fault never sets exit_code, and EXIT never
    sets a fault reason" -- refemu's `Halted` only ever passes `exit_code`
    on the EXIT path (`raise Halted(HaltReason.EXIT, pc, insn=insn,
    exit_code=e.code)`; every fault path leaves it at its `None` default,
    i.e. sqlcpu/executor's 0). `r.4.5` (this fold's internal `halt_extra`)
    still carries the raw word/faulting address for non-EXIT halts, same as
    before -- that's a real, useful diagnostic value, just not one SPEC §5's
    `cpu_state.exit_code` is the place for; it has no persisted slot in the
    ratified schema (refemu doesn't persist it either -- `insn`/`addr` live
    only on the transient `Halted` exception), so it is computed but not
    flushed anywhere by this design."""
    # PREV.4 (the batch's starting icount) reaches the fold only through
    # INIT_ACC's seed for acc.5, never interpolated into the lambda body
    # itself -- that placement is the fix for a real bug (build_step()'s
    # docstring): a per-batch-varying value baked into the lambda's own SQL
    # text defeats ClickHouse's compiled-expression cache (it never warms,
    # since the cache key includes the lambda's literal text and that text
    # changed every batch) -- ~1.23x on this real fold once fixed, measured
    # (build_step()'s docstring has the full before/after and the
    # system.query_log evidence that compilation actually engaged).
    step = build_step(K, text_start_widx, text_end_widx, decn, ram_words, hwm=hwm, ipms=ipms)
    init = INIT_ACC.format(pc0="assumeNotNull(PREV.2)", regs0="CAST(PREV.3, 'Array(UInt32)')",
                           keyq0="assumeNotNull(PREV.5)", icount0="assumeNotNull(PREV.4)")
    halt_reason_expr = _halt_reason_transform("r.4.3")
    exit_code_expr = f"if(toUInt8(r.4.3) = {config.HALT_EXIT}, r.4.5, toUInt32(0))"
    # The fold goes in a subquery and every column is projected off `r`
    # outside it -- NOT `(arrayFold(...) AS r).1, r.2, ...` in one SELECT list.
    # ClickHouse does not common-subexpression the alias in that position: each
    # reference re-runs the entire fold. Measured at K=1 on the real ROM, where
    # the fold itself is nearly free and the number is almost pure overhead:
    # 16.95s inline vs 1.70s wrapped, a 10x difference in fixed per-batch cost,
    # with identical results (#86). This reshape adds more outer-SELECT
    # references (halt_reason_expr, exit_code_expr, the icount sum) than the
    # version #86 fixed -- still safe, because they all read the single `r`
    # alias materialized by the subquery, same as every other column here;
    # what #86 forbids is aliasing the fold call itself inside the SELECT list.
    return f"""INSERT INTO {db}.batch_commit
  (batch_id, icount, pc, regs, halted, halt_reason, exit_code,
   keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount,
   fb_wl_addr, fb_wl_val, fb_wl_icount, pal_wl_addr, pal_wl_val, pal_wl_icount,
   console_bytes)
WITH{decode_with(db)},
  (SELECT tuple(batch_id, pc, regs, icount, keyq_pos)
     FROM {db}.batch_commit ORDER BY batch_id DESC LIMIT 1) AS PREV
SELECT toUInt64(assumeNotNull(PREV.1) + 1) AS batch_id,
       r.5 AS icount,
       r.1 AS pc, r.2 AS regs,
       r.4.2 AS halted, {halt_reason_expr} AS halt_reason, {exit_code_expr} AS exit_code,
       r.6.2 AS keyq_pos, r.6.3.2 AS has_frame, r.6.3.1 AS frame_no,
       r.3.1 AS wl_addr, r.3.2 AS wl_val, r.3.3 AS wl_icount,
       -- #130/#160: acc.7 was ALREADY computed on every step (this fold
       -- pays for it regardless -- see build_step()'s acc.7 comment); this
       -- was the missing projection, not new fold work. select_only()
       -- projects the same r.7.* fields already, for tests -- batch()
       -- just hadn't caught up.
       r.7.1 AS fb_wl_addr, r.7.2 AS fb_wl_val, r.7.3 AS fb_wl_icount,
       r.7.4 AS pal_wl_addr, r.7.5 AS pal_wl_val, r.7.6 AS pal_wl_icount,
       r.6.1 AS console_bytes
FROM (SELECT arrayFold((acc, i) -> {step}, range({K}), {init}) AS r)
SETTINGS max_threads = 1,
         -- The step expression is generated, not hand-written, and MMIO (#24)
         -- pushed it past ClickHouse's 50,000-node default. This raises a
         -- *limit*, it does not change what is computed: measured AST size is
         -- ~90k nodes and the ceiling is a guard against runaway generated
         -- SQL, which this is not. Node count still costs time under
         -- ADR-0001's model -- that is tracked as throughput, not as a limit.
         max_ast_elements = 500000, max_expanded_ast_elements = 500000,
         -- #130's FRAMEBUFFER/PALETTE lane pushed the generated query text
         -- itself (not just its parsed AST) past ClickHouse's 262,144-byte
         -- default max_query_size -- same "raises a limit, doesn't change
         -- what's computed" reasoning as the AST settings above; the SQL
         -- text is ~440KB, comfortably under this raised ceiling.
         max_query_size = 2000000"""


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
