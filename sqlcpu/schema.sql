-- sqlcpu/schema.sql — authoritative DDL for ClickDOOM emulator state.
--
-- SPEC §5 defines the shape of cpu_state, ram, input_queue, frames_out and
-- console_out; this file is where that shape becomes real DDL. It also adds
-- the pre-decoded instruction table ADR-0002 calls for — decode is a table,
-- not an expression, because arrayFold costs ~0.8us per expression node
-- almost independent of the data touched (docs/adr/0002-predecoded-instruction-table.md).
--
-- This is CI's marker file: `.github/workflows/ci.yml`'s test-sqlcpu job runs
-- `sqlcpu/run_tests.sh` unconditionally the moment this file exists on a PR
-- branch, with no guard of its own on run_tests.sh's presence. That script
-- lands in the same PR as this one so no PR — from any workstream — sees a
-- broken test-sqlcpu job in between.
--
-- Idempotent: safe to re-run against an already-provisioned database.
--
-- ---------------------------------------------------------------------------
-- Two ClickHouse (26.3) traps hit writing sqlcpu/run_riscv_tests.py's
-- density guard (#93/#98) that anyone writing NEW fold/flush/query SQL
-- against this schema -- executor's batch loop and commit flush chief among
-- them -- will hit too if they aren't warned. Both are silent-wrong-answer
-- shaped, not error-shaped, which is this project's signature failure mode.
--
-- 1. An unreferenced WITH-bound scalar subquery is never evaluated.
--    `WITH (SELECT throwIf(...) FROM t) AS guard, ...` does NOT run `guard`
--    unless something downstream actually references it -- ClickHouse
--    prunes it as dead. A guard/assertion written this way silently never
--    fires. Verified directly: a density check written exactly this way
--    produced zero error against a deliberately-broken input, with nothing
--    in the query plan or output hinting it hadn't run. Fix: consume the
--    alias somewhere the optimizer can't prune, e.g. folded into an
--    arrayFold's own init-tuple arithmetic (`initial_value + guard`, where
--    `guard` is 0 on success) rather than left dangling in the WITH clause.
--
-- 2. A scalar subquery's result is Nullable, regardless of whether the
--    underlying expression is. `(SELECT throwIf(...) FROM t)` has type
--    Nullable(UInt8) even though `throwIf` itself returns plain UInt8 --
--    and a single Nullable value anywhere in an arrayFold accumulator's
--    initial tuple poisons the WHOLE tuple's inferred type against the
--    step lambda's non-Nullable return, failing every row with a
--    TYPE_MISMATCH before a single instruction executes. Fix: wrap with
--    `assumeNotNull(...)`, same as this project's Phase 0 guidance for any
--    other scalar-subquery-derived value entering an accumulator.
-- ---------------------------------------------------------------------------

CREATE DATABASE IF NOT EXISTS clickdoom;

-- ---------------------------------------------------------------------------
-- Register file convention (binds #18 decode, #19 execute, and executor's
-- accumulator design — settle deviations with `executor` before relying on
-- them, not after).
--
-- `regs` holds x1..x31 only (31 elements, 1-indexed: regs[1] = x1's value,
-- regs[31] = x31's value). x0 is NEVER stored. Reading register r: if r = 0
-- the value is the UInt32 literal 0; otherwise regs[r]. Writing register r:
-- if r = 0 the write is discarded entirely (SPEC §1 — "the SQL register
-- file must enforce writes to x0 being discarded"). Both branches are
-- execute-expression responsibilities (#19), not something this schema can
-- enforce by itself — a 31-element array has no slot for x0 to be enforced
-- against.
-- ---------------------------------------------------------------------------

-- The batch's single atomic write (SPEC §5, §6; ADR-0003). One row per
-- committed batch, superset of cpu_state's columns plus everything crash
-- recovery needs to idempotently re-derive `ram` and `console_out`:
--   keyq_pos       cumulative KEYQ pops through this batch (SPEC §3.2) --
--                  input_queue consumption is a computed predicate against
--                  this, not a mutated `consumed` flag, so it needs no write
--                  of its own.
--   has_frame/frame_no  FRAME_COMMIT (SPEC §3), if any, this batch.
--   wl_addr/wl_val/wl_icount  the write-log: word_addr (RAM_BASE-relative,
--                  per executor's fold.py convention -- the flush into
--                  clickdoom.ram, which is ABSOLUTE word_addr, must add
--                  RAM_BASE_WORD back on; see #81), the stored value, and
--                  the store's OWN icount as that delta's `ram.version` --
--                  never the batch's final icount, which would tie two
--                  same-address stores in one batch under ram's
--                  ReplacingMergeTree and violate SPEC §8's explicit-
--                  ordering rule.
--   console_bytes  PUTCHAR bytes emitted this batch, flushed into
--                  console_out in (batch_id, array position) order.
--
-- Bounded by retention on batch_id LAG, never wall-clock time (ADR-0003's
-- rejected-TTL writeup: a wall-clock TTL loses the last committed batch's
-- write-log if the driver is down longer than the window, which is not an
-- edge case for a multi-week demo3 run -- it silently and permanently
-- diverges `ram` from `cpu_state` with nothing to reconcile from).
-- Retention drops entire rows -- whole batch_ids, thin columns and bulky
-- columns together -- via a fixed statement (partition-drop, or `DELETE
-- WHERE batch_id < (SELECT max(batch_id) - N FROM batch_commit)`) the
-- driver issues unconditionally every batch; N = 16 (executor/config). That
-- is executor's driver-loop housekeeping (PURITY.md action 4: computes
-- nothing, the threshold is computed in SQL), not this file's concern --
-- this file only has to make row-level retention possible, which a plain
-- MergeTree already is. Never selectively null the bulky columns on old
-- rows in place: that is a second write this design doesn't need and SPEC
-- §5 doesn't ask for.
CREATE TABLE IF NOT EXISTS clickdoom.batch_commit
(
    spec_version String DEFAULT '0.1.0',
    batch_id     UInt64,
    icount       UInt64,
    pc           UInt32,
    regs         Array(UInt32),  -- len 31: x1..x31, see convention above
    halted       UInt8,
    halt_reason  LowCardinality(String),
    exit_code    UInt32,
    keyq_pos     UInt64,
    has_frame    UInt8,
    frame_no     UInt32,
    wl_addr      Array(UInt32),
    wl_val       Array(UInt32),
    wl_icount    Array(UInt64),
    console_bytes Array(UInt8)
)
ENGINE = MergeTree
ORDER BY batch_id;

-- One row per committed batch (SPEC §5, SPEC §6), holding exactly cpu_state's
-- historical seven columns. A durable table, NEVER pruned -- unlike
-- batch_commit above, whose bulky write-log/console columns are windowed to
-- the last N batches. Populated from batch_commit by the same idempotent
-- flush that populates `ram` and `console_out` (ADR-0003): a fourth
-- derivation on identical terms, not a special case. ReplacingMergeTree
-- keyed by batch_id so a flush redone after a crash cannot leave two rows
-- for one batch -- the derivation is a pure function of one batch_commit
-- row, so any duplicate pair is content-identical regardless of merge
-- state, and does not need FINAL to read correctly; FINAL only matters if
-- the row *count* or the full unmerged history is what's being inspected.
-- An earlier draft made this a VIEW over batch_commit -- rejected on review
-- (#39): batch_commit's row-level retention would have silently turned this
-- table from an unbounded log into a rolling N-row window, which is a real
-- behavior change to a contracted table even though the column shape was
-- preserved. The row with max(batch_id) is the current state, reloaded at
-- the start of the next batch.
CREATE TABLE IF NOT EXISTS clickdoom.cpu_state
(
    spec_version String DEFAULT '0.1.0',
    batch_id     UInt64,
    icount       UInt64,
    pc           UInt32,
    regs         Array(UInt32),  -- len 31: x1..x31, see convention above
    halted       UInt8,
    halt_reason  LowCardinality(String),
    exit_code    UInt32
)
ENGINE = ReplacingMergeTree
ORDER BY batch_id;

-- RAM, word-addressed (byte address >> 2), SPEC §2's 24 MiB region.
-- ReplacingMergeTree so a store amends the previous value at that word;
-- `version` = icount of the store, so the last writer always wins under
-- FINAL. Read once per batch as a captured constant array — materialize
-- with FINAL, not `argMax(...) GROUP BY word_addr`: Phase 0 measured
-- 0.022-0.030s against 0.245-0.256s for the argMax form, and FINAL stayed
-- flat with 1.2M accumulated deltas (docs/adr/0001-batch-execution-with-arrayfold.md).
CREATE TABLE IF NOT EXISTS clickdoom.ram
(
    spec_version String DEFAULT '0.1.0',
    word_addr    UInt32,
    value        UInt32,
    version      UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

-- Key events. The driver INSERTs raw events here (PURITY.md action 2, the
-- only computation-free way input reaches the CPU). KEYQ (SPEC §3.2) pops
-- the oldest row with consumed = 0, ordered by event_seq — that ordering is
-- load-bearing (SPEC §8.2): never rely on block order to find "the next"
-- event.
CREATE TABLE IF NOT EXISTS clickdoom.input_queue
(
    spec_version String DEFAULT '0.1.0',
    event_seq    UInt64,
    key_event    UInt16,
    consumed     UInt8
)
ENGINE = MergeTree
ORDER BY event_seq;

-- Frames, written by the render query on FRAME_COMMIT (SPEC §3, §5). One row
-- per committed frame; frame_no is the value the ROM wrote to FRAME_COMMIT.
CREATE TABLE IF NOT EXISTS clickdoom.frames_out
(
    spec_version     String DEFAULT '0.1.0',
    frame_no         UInt32,
    committed_icount UInt64,
    fb               String,  -- 64,000 bytes: 320x200, 8bpp palette-indexed, row-major
    palette          String   -- 768 bytes: 256 x RGB (3 bytes each)
)
ENGINE = MergeTree
ORDER BY frame_no;

-- Debug console bytes (PUTCHAR, SPEC §3). One row per byte written; seq
-- gives the emission order for readout.
--
-- ReplacingMergeTree, not plain MergeTree (agreed with executor, #25):
-- console_out is flushed from batch_commit.console_bytes the same
-- idempotent-redo-safe way ram/cpu_state are (ADR-0003) -- a flush
-- re-run after a crash must not append duplicate bytes. `seq` is the key
-- rather than a separate `consumed`-style write, per ADR-0003 point 4 ("the
-- cheapest way to make a write atomic is to not have the write").
--
-- seq = bitShiftLeft(batch_id, 32) + array_position (array_position =
-- this byte's 0-indexed position within its batch's console_bytes array,
-- so the value is monotonic in emission order within a batch by
-- construction). NOT `batch_id * STRIDE + array_position` for a fixed
-- STRIDE -- an earlier draft used that and it collides silently the
-- moment K is tuned so one batch's console output exceeds STRIDE bytes
-- (ReplacingMergeTree just keeps one of the two colliding rows, no
-- error). Reserving the full low 32 bits for array_position instead of a
-- fixed STRIDE needs no such cap: a batch retiring at most K (<=200,000
-- tested, SPEC §6) instructions cannot emit anywhere near 2^32 PUTCHARs in
-- one batch, and batch_id staying under 2^32 (demo3 is ~48,500 batches at
-- K=50,000, nine orders of magnitude of headroom) is what keeps the shift
-- itself from losing bits. Verified no aliasing is possible under either
-- bound.
--
-- FINAL is required for a correct read, same as ram/cpu_state above: an
-- un-FINAL'd read after a redone flush can return the duplicate row a
-- crash-recovery replay produces before the next merge collapses it, and
-- unlike ram's "duplicates are content-identical so it's harmless either
-- way," a duplicate row here means the SAME byte is read TWICE at
-- adjacent-looking positions in an `ORDER BY seq` scan -- a real corrupted
-- readout (a repeated character), not a merely-redundant one. Every
-- console_out reader (the debug readout query, any future divergence
-- tooling) MUST read with FINAL; this is not optional the way it can be
-- for a one-off row-count check elsewhere in this file.
CREATE TABLE IF NOT EXISTS clickdoom.console_out
(
    spec_version String DEFAULT '0.1.0',
    seq          UInt64,
    byte         UInt8
)
ENGINE = ReplacingMergeTree
ORDER BY seq;

-- ---------------------------------------------------------------------------
-- Pre-decoded instruction table (ADR-0002). Built by a single SQL statement
-- over clickdoom.ram's text region (#18) — decoding happens INSIDE
-- ClickHouse, which PURITY.md explicitly allows ("Decoding the ROM inside
-- ClickHouse into a decoded-instruction table is fine — that's SQL doing the
-- work. Doing it in Python and inserting the result is not."). Keyed by
-- word_addr so pc>>2 looks a row up directly — same key domain as `ram`,
-- deliberately, so no separate index translation is needed at execute time.
--
-- Column semantics:
--   id   dense collapsed opcode (dispatch key for the execute multiIf, see
--        below). Agreed with executor (PR #48) — ids 0..27 are the Phase 0
--        bench's numbering; 28..31 are new, added for SPEC §1's fatal-halt
--        arms (ecall/ebreak/CSR/illegal), which the bench never needed
--        since its decode table never halted anything.
--   rd   destination register number, 0..31 (0 = no write; RD!=0 is checked
--        at execute time regardless, since a real encoded rd of x0, e.g.
--        `addi x0,x0,0` as nop, must also discard its write)
--   rs1  first source register number, 0..31
--   rs2  second source register number, 0..31; the decoder sets this to 0
--        for I-type instructions (see the ALU-arm collapse note below)
--   imm  sign-extended immediate for I/S/B/J-type instructions; for the
--        instructions that collapse onto the `add` arm with rs1=rs2=0
--        (lui, auipc — see below) this holds the fully-precomputed constant
--        each of those instructions writes to rd, computed at decode time
--        from word_addr since pc is static per decoded row
--   tgt  absolute target BYTE ADDRESS for branches and jal — precomputed at
--        decode time since both are pc-relative and pc is static per row.
--        Deliberately NOT pre-shifted to a word index: an early version of
--        this column stored `tgt >> 2`, which silently discards a set bit 1
--        (a target that's 2-byte aligned but not 4-byte aligned — the
--        encodings only force bit 0 to 0, not bit 1) instead of leaving it
--        detectable as a SPEC §1 MISALIGNED condition (agreed with refemu,
--        issue #37). NOT used for jalr (register-relative, computed live)
--        and NOT a link value: the link value jal/jalr write to rd is
--        pc + 4, which the execute expression computes directly from the
--        accumulator's live pc rather than storing it here. (The Phase 0
--        bench's placeholder `tgt` column conflated "jump target" and "link
--        value" into one field for the same id — harmless there since the
--        bench's decode table is synthetic and never executed, see
--        executor/bench/phase0/RESULTS.md §6, but it would have been a
--        real bug in a table meant to produce correct results, so this
--        schema splits the concern by not storing the link value at all.)
--   mk   load/store width mask (0xFF / 0xFFFF / 0xFFFFFFFF); meaningful
--        only for id = 18 (load) and id = 19 (store)
--   sg   sign-extend flag for loads (1 = sign-extend, 0 = zero-extend);
--        meaningful only for id = 18 (load) — stores never sign-extend
--   raw  the undecoded instruction word, kept only so an id = 31
--        (ILLEGAL_INSN) halt record can report the actual bad instruction
--        (SPEC §1) without re-reading `ram` at halt time. Added at
--        executor's request (PR #48); unused for every other id.
--
-- Two collapses fold most of the opcode space onto fewer arms (ADR-0002):
--   * I-type and R-type share one arm each: the decoder sets rs2 = 0 and
--     imm = 0 respectively (whichever the encoding doesn't carry), so
--     `b = regs[rs2] + imm` is the right second operand either way with no
--     branch in the execute expression. addi/add become the same arm, etc.
--   * lui, auipc collapse onto the same `add` arm (id = 0) with rs1 = rs2 = 0
--     and the constant each writes to rd precomputed into `imm` (auipc's
--     constant is pc-relative, computable at decode time from word_addr).
--
-- id assignment (agreed with executor, PR #48):
--   0 add   1 sub   2 sll   3 slt   4 sltu  5 xor   6 srl   7 sra
--   8 or    9 and   10 mul  11 mulh 12 mulhsu 13 mulhu
--   14 div  15 divu 16 rem  17 remu
--   18 load 19 store
--   20 beq  21 bne  22 blt  23 bge  24 bltu 25 bgeu
--   26 jal  27 jalr
--   28 ecall  29 ebreak  30 csr  31 illegal/unimplemented
-- lui/auipc carry no id of their own — they decode as id = 0 (add), per the
-- collapse above. FENCE/FENCE.I likewise carry no id of their own — single
-- in-order hart, no cache, so they decode as id = 0 too (rd forced to 0
-- makes it a true no-op), agreed with refemu (#37) rather than treated as
-- illegal.
--
-- Halt-reason strings for 28..31, agreed with refemu (#37): ECALL, EBREAK,
-- CSR, ILLEGAL_INSN — produced by execute (#19), not stored here.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS clickdoom.decoded
(
    spec_version String DEFAULT '0.1.0',
    word_addr    UInt32,
    id           UInt8,
    rd           UInt8,
    rs1          UInt8,
    rs2          UInt8,
    imm          UInt32,
    tgt          UInt32,
    mk           UInt32,
    sg           UInt8,
    raw          UInt32
)
ENGINE = MergeTree
ORDER BY word_addr;
