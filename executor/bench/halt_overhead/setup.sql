-- #23 throughput evidence: same synthetic DOOM-shaped mix as
-- executor/bench/phase0/setup.sql (same fractions, same deterministic hash
-- of `number`, same 24 MiB RAM / 2 MiB text sizing), rebuilt against the
-- SPEC §5-shaped fixture (executor/schema_fixture.sql) so the comparison is
-- "same instruction stream, old fold vs. new fold with halt semantics."
--
-- Not a correctness harness (see executor/bench/phase0/setup.sql's own
-- disclaimer) -- and per Phase 0's finding that arrayFold's `if`/`multiIf`
-- never short-circuit, whether any of these synthetic instructions would
-- "actually" halt doesn't change the node count evaluated per step, so this
-- reuses Phase 0's fixture generator unmodified rather than hand-crafting a
-- halt-free stream.

DROP TABLE IF EXISTS clickdoom_executor.ram;
CREATE TABLE clickdoom_executor.ram
(
    word_addr UInt32,
    value     UInt32,
    version   UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

INSERT INTO clickdoom_executor.ram
SELECT toUInt32(2147483648 + number), toUInt32(number * 2654435761), 0
FROM numbers(6291456);          -- 24 MiB / 4

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

INSERT INTO clickdoom_executor.decoded
SELECT
    toUInt32(2147483648 + number) AS word_addr,
    multiIf(m < 35, toUInt8(number % 10),
            m < 60, 18,
            m < 70, 19,
            m < 88, toUInt8(20 + number % 6),
            m < 93, toUInt8(26 + number % 2),
                    toUInt8(10 + number % 8)) AS op_id,
    toUInt8(1 + number % 31)                  AS rd,
    toUInt8(number % 32)                      AS rs1,
    toUInt8((number * 7) % 32)                AS rs2,
    toUInt32((number * 2654435761) % 4096)    AS imm,
    toUInt32((number * 40503) % 524288)       AS target,  -- decode-array (pcidx) space, not a memory address
    multiIf(op_id = 18 OR op_id = 19, arrayElement([255, 65535, 4294967295], toUInt8(1 + number % 3)), 4294967295) AS width_mask,
    multiIf(op_id = 18,               arrayElement([128, 32768, 0],          toUInt8(1 + number % 3)), 0)          AS sign_bit,
    toUInt32(number)                          AS raw
FROM (SELECT number, (number * 2654435761) % 100 AS m FROM numbers(524288));

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
    batch_id      UInt64,
    icount_before UInt64,
    pcidx         UInt32,
    regs          Array(UInt32),
    wl_addr       Array(UInt32),
    wl_val        Array(UInt32),
    wl_icount     Array(UInt64),
    stopped       UInt8,
    halted        UInt8,
    halt_reason   UInt8,
    halt_pc       UInt32,
    halt_extra    UInt32,
    retired       UInt32
)
ENGINE = MergeTree ORDER BY batch_id;
