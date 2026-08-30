-- A1 fixture: deterministic tables for the JIT-compilability probes.
-- Seeded entirely from `number` -- no now(), no rand() (SPEC §8.1).  -- purity-ok: documents the absence of now()/rand(), doesn't call either
--
-- `decoded`/`ram`/`input_queue` mirror sqlcpu/schema.sql column-for-column
-- so `python3 executor/fold.py K --db a1_jit_bench` (the REAL production
-- step expression) runs unmodified against this database.
DROP DATABASE IF EXISTS a1_jit_bench;
CREATE DATABASE a1_jit_bench;

CREATE TABLE a1_jit_bench.decoded
(
    word_addr UInt32,
    id UInt8, rd UInt8, rs1 UInt8, rs2 UInt8,
    imm UInt32, tgt UInt32, mk UInt32, sg UInt8, raw UInt32
) ENGINE = MergeTree ORDER BY word_addr;

-- 524,288 words = 2 MiB of text, matching the baseline fold and E1 fixture.
-- id is kept in 0..19 (pure ALU ops) so the synthetic "program" never halts:
-- a halted fold would short-circuit the batch and destroy the measurement.
INSERT INTO a1_jit_bench.decoded
SELECT
    toUInt32(number)                            AS word_addr,
    toUInt8(number % 20)                        AS id,
    toUInt8(number % 32)                        AS rd,
    toUInt8((number * 7) % 32)                  AS rs1,
    toUInt8((number * 13) % 32)                 AS rs2,
    toUInt32((number * 2654435761) % 4096)      AS imm,
    toUInt32(2147483648 + (number % 1024) * 4)  AS tgt,
    toUInt32(4294967295)                        AS mk,
    toUInt8(0)                                  AS sg,
    toUInt32(number * 2246822519)               AS raw
FROM numbers(524288);

CREATE TABLE a1_jit_bench.ram
(
    word_addr UInt32, value UInt32, version UInt64
) ENGINE = ReplacingMergeTree(version) ORDER BY word_addr;

INSERT INTO a1_jit_bench.ram
SELECT toUInt32(number), toUInt32(number * 2654435761), toUInt64(0)
FROM numbers(524288);

CREATE TABLE a1_jit_bench.input_queue
(
    event_seq UInt64, key_event UInt16, consumed UInt8
) ENGINE = MergeTree ORDER BY event_seq;

INSERT INTO a1_jit_bench.input_queue
SELECT toUInt64(number), toUInt16(number % 256), toUInt8(0) FROM numbers(64);

OPTIMIZE TABLE a1_jit_bench.decoded FINAL;
OPTIMIZE TABLE a1_jit_bench.ram FINAL;
