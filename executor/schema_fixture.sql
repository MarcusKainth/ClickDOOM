-- Local test schema for #23, mirroring sqlcpu/schema.sql (PR #42/#46/#49)
-- exactly, not just SPEC §5's prose -- this file exists only so the fold in
-- fold.py can be built and tested before that PR merges, per #23's plan
-- ("build to spec, not to the fixture -- swap the moment schema.sql lands").
-- Column names (`id`/`tgt`/`mk`/`sg`, not SPEC §5's `op_id`/`target`/
-- `width_mask`/`sign_bit`) and the byte-address `pc` convention match
-- sqlcpu's real table exactly, per their request to reconcile this in the
-- same pass as the word-index-PC fix (see fold.py's IDX/NEXT/LINK_VALUE).
--
-- One deliberate addition beyond sqlcpu's schema.sql: `state`/`batch_out`
-- are NOT part of SPEC §5 or sqlcpu's schema -- they're this fixture's
-- stand-in for "the previous batch's committed state" until #25
-- (batch_commit) is ratified, matching Phase 0's shape plus the new
-- halt/write-log-versioning fields.
--
-- `word_addr` is absolute (byte_addr >> 2) per SPEC §5's parenthetical, not
-- relative to RAM_BASE, matching what sqlcpu's real table contains.

CREATE DATABASE IF NOT EXISTS clickdoom_executor;

DROP TABLE IF EXISTS clickdoom_executor.ram;
CREATE TABLE clickdoom_executor.ram
(
    word_addr UInt32,
    value     UInt32,
    version   UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

DROP TABLE IF EXISTS clickdoom_executor.decoded;
CREATE TABLE clickdoom_executor.decoded
(
    word_addr UInt32,
    id        UInt8,
    rd        UInt8,
    rs1       UInt8,
    rs2       UInt8,
    imm       UInt32,
    tgt       UInt32,
    mk        UInt32,
    sg        UInt8,
    raw       UInt32
)
ENGINE = MergeTree
ORDER BY word_addr;

DROP TABLE IF EXISTS clickdoom_executor.state;
CREATE TABLE clickdoom_executor.state
(
    batch_id UInt64,
    pc       UInt32,  -- byte address, matching SPEC §5's cpu_state.pc
    regs     Array(UInt32),
    icount   UInt64
)
ENGINE = MergeTree ORDER BY batch_id;

DROP TABLE IF EXISTS clickdoom_executor.batch_out;
CREATE TABLE clickdoom_executor.batch_out
(
    batch_id      UInt64,
    icount_before UInt64,
    pc            UInt32,  -- byte address
    regs          Array(UInt32),
    wl_addr       Array(UInt32),
    wl_val        Array(UInt32),
    wl_icount     Array(UInt64),
    stopped       UInt8,
    halted        UInt8,
    halt_reason   UInt8,
    halt_pc       UInt32,  -- byte address
    halt_extra    UInt32,
    retired       UInt32
)
ENGINE = MergeTree ORDER BY batch_id;
