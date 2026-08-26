-- Isolated-database counterpart to executor/bench/halt_overhead/setup.sql.
-- `ram`/`decoded`'s DDL comes from the REAL sqlcpu/schema.sql (renamed onto
-- this bench's private database by run.sh, via sed) rather than a
-- hand-copied approximation, so it can't drift from what sqlcpu maintains --
-- team lead's ruling after a batch-overhead run got silently corrupted by
-- concurrent DDL on the shared `clickdoom_executor` tables (some other
-- process -- almost certainly a riscv-tests iteration -- issued 101
-- TRUNCATE/CREATE/DROP cycles against those exact tables mid-run; caught via
-- `batch_out.retired` not matching K*BATCHES, root-caused via
-- system.query_log). This file only adds what the real schema doesn't have.
--
-- `state`/`batch_out` are NOT part of sqlcpu's schema (same as
-- executor/schema_fixture.sql's note) -- they're this benchmark's stand-in
-- for "the previous batch's committed state" until #25 (batch_commit) is
-- ratified. `{{DB}}` is substituted by run.sh with the bench's private
-- database name -- never run this file unsubstituted.

DROP TABLE IF EXISTS {{DB}}.state;
CREATE TABLE {{DB}}.state
(
    batch_id UInt64,
    pc       UInt32,  -- byte address, matching SPEC §5's cpu_state.pc
    regs     Array(UInt32),
    icount   UInt64,
    keyq_pos UInt32   -- cumulative KEYQ pops (SPEC §3.2); carried across batches
)
ENGINE = MergeTree ORDER BY batch_id;

DROP TABLE IF EXISTS {{DB}}.batch_out;
CREATE TABLE {{DB}}.batch_out
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
    retired       UInt32,
    -- SPEC §3 MMIO side effects, produced by the fold's acc.6
    console_bytes   Array(UInt8),
    keyq_pos        UInt32,
    frame_no        UInt32,
    frame_committed UInt8
)
ENGINE = MergeTree ORDER BY batch_id;

-- Same synthetic DOOM-shaped mix as executor/bench/halt_overhead/setup.sql
-- (same fractions, same deterministic hash of `number`, same safety fixes:
-- load/store/jalr force rs1=0 and an unconditionally in-bounds, aligned,
-- non-text address/target -- see that file's comment for why). Explicit
-- column lists below, not `INSERT INTO ... SELECT` bare, because the real
-- schema's `ram`/`decoded` carry a `spec_version` column (DEFAULT'd) this
-- mix doesn't need to set.
TRUNCATE TABLE {{DB}}.ram;
INSERT INTO {{DB}}.ram (word_addr, value, version)
SELECT toUInt32(2147483648 + number), toUInt32(number * 2654435761), 0
FROM numbers(6291456);          -- 24 MiB / 4

TRUNCATE TABLE {{DB}}.decoded;
INSERT INTO {{DB}}.decoded (word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg, raw)
SELECT
    toUInt32(2147483648 + number) AS word_addr,
    id,
    toUInt8(1 + number % 31)                  AS rd,
    multiIf(id IN (18, 19, 27), 0, toUInt8(number % 32)) AS rs1,
    toUInt8((number * 7) % 32)                AS rs2,
    multiIf(id IN (18, 19),
            toUInt32(2147483648 + 2097152 + ((number * 4) % (6291456 * 4 - 2097152 - 4))),
            id = 27,
            toUInt32(2147483648 + ((number * 40503 + 12345) % 524288) * 4),
            toUInt32((number * 2654435761) % 4096)) AS imm,
    toUInt32(2147483648 + ((number * 40503) % 524288) * 4) AS tgt,  -- byte address, always 4-aligned
    multiIf(id = 18 OR id = 19, arrayElement([255, 65535, 4294967295], toUInt8(1 + number % 3)), 4294967295) AS mk,
    multiIf(id = 18, toUInt8(number % 2), 0) AS sg,  -- boolean sign-extend flag (sqlcpu's convention)
    toUInt32(number)                          AS raw
FROM (SELECT number, m,
             multiIf(m < 35, toUInt8(number % 10),
                     m < 60, 18,
                     m < 70, 19,
                     m < 88, toUInt8(20 + number % 6),
                     m < 93, toUInt8(26 + number % 2),
                             toUInt8(10 + number % 8)) AS id
      FROM (SELECT number, (number * 2654435761) % 100 AS m FROM numbers(524288)));

TRUNCATE TABLE {{DB}}.state;
TRUNCATE TABLE {{DB}}.batch_out;
