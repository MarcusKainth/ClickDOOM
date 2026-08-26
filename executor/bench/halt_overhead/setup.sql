-- #23 throughput evidence: same synthetic DOOM-shaped mix as
-- executor/bench/phase0/setup.sql (same fractions, same deterministic hash
-- of `number`, same 24 MiB RAM / 2 MiB text sizing), against the fixture
-- schema (executor/schema_fixture.sql, run.sh applies it first) so the
-- comparison is "same instruction stream, old fold vs. new fold with halt
-- semantics." Only INSERTs live here -- the table DDL is schema_fixture.sql's
-- alone now, not duplicated, so the two can't silently drift apart the way
-- they briefly did across the PC-representation rework (target went from a
-- word index to a byte address, and columns were renamed to match sqlcpu's
-- id/tgt/mk/sg).
--
-- Not a correctness harness (see executor/bench/phase0/setup.sql's own
-- disclaimer) -- and per Phase 0's finding that arrayFold's `if`/`multiIf`
-- never short-circuit, whether any of these synthetic instructions would
-- "actually" halt doesn't change the node count evaluated per step, so this
-- reuses Phase 0's fixture generator largely unmodified rather than
-- hand-crafting a halt-free stream. `target` is generated as a byte address
-- (word_index * 4), which is always 4-byte aligned regardless of
-- word_index's value, so the new eager MISALIGNED check never fires here
-- either -- the mix stays halt-free for the same reason it always was.

TRUNCATE TABLE clickdoom_executor.ram;
INSERT INTO clickdoom_executor.ram
SELECT toUInt32(2147483648 + number), toUInt32(number * 2654435761), 0
FROM numbers(6291456);          -- 24 MiB / 4

TRUNCATE TABLE clickdoom_executor.decoded;
INSERT INTO clickdoom_executor.decoded
SELECT
    toUInt32(2147483648 + number) AS word_addr,
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
    toUInt32(2147483648 + ((number * 40503) % 524288) * 4) AS tgt,  -- byte address, always 4-aligned
    multiIf(id = 18 OR id = 19, arrayElement([255, 65535, 4294967295], toUInt8(1 + number % 3)), 4294967295) AS mk,
    multiIf(id = 18, toUInt8(number % 2), 0) AS sg,  -- boolean sign-extend flag (sqlcpu's convention)
    toUInt32(number)                          AS raw
FROM (SELECT number, (number * 2654435761) % 100 AS m FROM numbers(524288));

TRUNCATE TABLE clickdoom_executor.state;
TRUNCATE TABLE clickdoom_executor.batch_out;
