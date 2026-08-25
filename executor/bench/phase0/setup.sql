-- Phase 0 benchmark fixtures (SPEC §9).
--
-- This is a THROUGHPUT harness, not a correctness harness: it measures how
-- fast ClickHouse can turn the arrayFold crank on an RV32IM-shaped lambda.
-- The "ROM" is therefore pseudo-random words and the decode table is a
-- synthetic instruction mix, both generated deterministically from a
-- multiplicative hash of `number`, so nothing here touches wall clock or
-- randomness (SPEC §8).
--
-- Correctness is riscv-tests' job (sqlcpu workstream), not this file's.

CREATE DATABASE IF NOT EXISTS clickdoom_bench;

-- 24 MiB of RAM as words, in the SPEC §5 shape.
DROP TABLE IF EXISTS clickdoom_bench.ram;
CREATE TABLE clickdoom_bench.ram
(
    word_addr UInt32,
    value     UInt32,
    version   UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

INSERT INTO clickdoom_bench.ram
SELECT toUInt32(number), toUInt32(number * 2654435761), 0
FROM numbers(6291456);          -- 24 MiB / 4

-- Pre-decoded instruction table (ADR-0002), covering a 2 MiB text segment.
-- `id` is the collapsed opcode space; `imm` is already sign-extended; `tgt`
-- holds the absolute target word index for jal/branch and the link value for
-- jal/jalr; `mk`/`sg` are the load/store width mask and sign bit.
DROP TABLE IF EXISTS clickdoom_bench.decoded;
CREATE TABLE clickdoom_bench.decoded
(
    widx UInt32,
    id   UInt8,
    rd   UInt8,
    rs1  UInt8,
    rs2  UInt8,
    imm  UInt32,
    tgt  UInt32,
    mk   UInt32,
    sg   UInt32
)
ENGINE = MergeTree
ORDER BY widx;

-- Synthetic dynamic mix, roughly DOOM-shaped: ~35% ALU, ~25% load, ~10% store,
-- ~18% branch, ~5% jump, ~7% M-extension. The exact split matters less than it
-- looks: multiIf does not short-circuit on condition position (measured), so
-- every arm costs whether or not it is selected.
INSERT INTO clickdoom_bench.decoded
SELECT
    toUInt32(number) AS widx,
    multiIf(m < 35, toUInt8(number % 10),
            m < 60, 18,
            m < 70, 19,
            m < 88, toUInt8(20 + number % 6),
            m < 93, toUInt8(26 + number % 2),
                    toUInt8(10 + number % 8)) AS id,
    toUInt8(1 + number % 31)                  AS rd,
    toUInt8(number % 32)                      AS rs1,
    toUInt8((number * 7) % 32)                AS rs2,
    toUInt32((number * 2654435761) % 4096)    AS imm,
    toUInt32((number * 40503) % 524288)       AS tgt,
    multiIf(id = 18 OR id = 19, arrayElement([255, 65535, 4294967295], toUInt8(1 + number % 3)), 4294967295) AS mk,
    multiIf(id = 18,            arrayElement([128, 32768, 0],          toUInt8(1 + number % 3)), 0)          AS sg
FROM (SELECT number, (number * 2654435761) % 100 AS m FROM numbers(524288));

-- Batch-loop state, for the end-to-end measurement.
DROP TABLE IF EXISTS clickdoom_bench.state;
CREATE TABLE clickdoom_bench.state
(
    batch_id UInt64,
    pcidx    UInt32,
    regs     Array(UInt32),
    icount   UInt64
)
ENGINE = MergeTree ORDER BY batch_id;

DROP TABLE IF EXISTS clickdoom_bench.batch_out;
CREATE TABLE clickdoom_bench.batch_out
(
    batch_id UInt64,
    icount   UInt64,
    pcidx    UInt32,
    regs     Array(UInt32),
    wl_addr  Array(UInt32),
    wl_val   Array(UInt32)
)
ENGINE = MergeTree ORDER BY batch_id;
