-- E1 fixture: a deterministic pre-decoded text segment.
-- Seeded entirely from `number` -- no now(), no rand() (SPEC §8.1). -- purity-ok: documents the absence of now()/rand(), doesn't call either
DROP DATABASE IF EXISTS e1_cse_bench;
CREATE DATABASE e1_cse_bench;

CREATE TABLE e1_cse_bench.decoded
(
    word_addr UInt32,
    id  UInt32, rd  UInt32, rs1 UInt32, rs2 UInt32, imm UInt32,
    tgt UInt32, mk  UInt32, sg  UInt32, raw UInt32
) ENGINE = MergeTree ORDER BY word_addr;

-- 524,288 words = 2 MiB of text, matching the baseline fold benchmark's fixture.
-- rs1/rs2 land in [0, 31] so the guarded `regs[rs2]` read is in range for a
-- 31-element (x1..x31) register file, and hits the rs2 = 0 guard 1/32 of the
-- time -- i.e. both arms of the `if` are exercised.
INSERT INTO e1_cse_bench.decoded
SELECT
    toUInt32(number)                        AS word_addr,
    toUInt32(number % 28)                   AS id,
    toUInt32(number % 32)                   AS rd,
    toUInt32((number * 7) % 32)             AS rs1,
    toUInt32((number * 13) % 32)            AS rs2,
    toUInt32((number * 2654435761) % 4096)  AS imm,
    toUInt32(2147483648 + (number % 1024) * 4) AS tgt,
    toUInt32(4294967295)                    AS mk,
    toUInt32(0)                             AS sg,
    toUInt32(number * 2246822519)           AS raw
FROM numbers(524288);

OPTIMIZE TABLE e1_cse_bench.decoded FINAL;
