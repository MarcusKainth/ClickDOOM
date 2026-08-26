-- Local test schema for #23, mirroring SPEC §5's shape exactly where SPEC
-- defines it. sqlcpu/schema.sql (#17) is the authoritative DDL and owns
-- this table's real definition; this file exists only so the fold in
-- fold.py can be built and tested before #17 lands, per #23's plan
-- ("build to spec, not to the fixture -- swap the moment schema.sql lands").
--
-- One deliberate addition beyond SPEC §5's `decoded` prose: a `raw UInt32`
-- column, needed to populate the ILLEGAL_INSN halt record's "raw
-- instruction word" (SPEC §1) since the pre-decoded table (ADR-0002)
-- otherwise never retains it. Flagged to sqlcpu for schema.sql -- SPEC §5
-- describes decoded's shape in prose, not as a closed column list.
--
-- `word_addr` is absolute (byte_addr >> 2) per SPEC §5's parenthetical, not
-- relative to RAM_BASE, matching what sqlcpu's real table will contain.
-- `state` is NOT part of SPEC §5 -- it's this fixture's stand-in for
-- reading "the previous batch's committed state" until #25 (batch_commit)
-- is ratified. `batch_out` is likewise fixture-only staging, matching
-- Phase 0's shape plus the new halt/write-log-versioning fields.

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
    word_addr  UInt32,
    op_id      UInt8,
    rd         UInt8,
    rs1        UInt8,
    rs2        UInt8,
    imm        UInt32,
    target     UInt32,
    width_mask UInt32,
    sign_bit   UInt32,
    raw        UInt32
)
ENGINE = MergeTree
ORDER BY word_addr;

DROP TABLE IF EXISTS clickdoom_executor.state;
CREATE TABLE clickdoom_executor.state
(
    batch_id UInt64,
    pcidx    UInt32,
    regs     Array(UInt32),
    icount   UInt64
)
ENGINE = MergeTree ORDER BY batch_id;

DROP TABLE IF EXISTS clickdoom_executor.batch_out;
CREATE TABLE clickdoom_executor.batch_out
(
    batch_id     UInt64,
    icount_before UInt64,
    pcidx        UInt32,
    regs         Array(UInt32),
    wl_addr      Array(UInt32),
    wl_val       Array(UInt32),
    wl_icount    Array(UInt64),
    stopped      UInt8,
    halted       UInt8,
    halt_reason  UInt8,
    halt_pc      UInt32,
    halt_extra   UInt32,
    retired      UInt32
)
ENGINE = MergeTree ORDER BY batch_id;
