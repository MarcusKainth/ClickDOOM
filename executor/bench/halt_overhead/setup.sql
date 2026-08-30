-- #23 throughput evidence: same synthetic DOOM-shaped mix as
-- executor/bench/phase0/setup.sql and executor/bench/batch_overhead/setup.sql
-- (same fractions, same deterministic hash of `number`, same 24 MiB RAM /
-- 2 MiB text sizing, same load/store/jalr safety fixes below) -- three
-- copies of the same mix now, not one, because each bench predates the
-- next's isolated-database rework and none of them share a fixture file.
-- Not attempting to de-duplicate here; flagging so a future edit to one
-- knows to check the other two rather than assume this is the only copy.
--
-- `ram`/`decoded`/`batch_commit`/`cpu_state`/`console_out`/`input_queue`'s
-- DDL all comes from the REAL sqlcpu/schema.sql (renamed onto this bench's
-- private database by run.sh, via sed), same as batch_overhead's -- this
-- file only adds the synthetic mix. `{{DB}}` is substituted by run.sh with
-- the bench's private database name -- never run this file unsubstituted.
--
-- Not a correctness harness (see executor/bench/phase0/setup.sql's own
-- disclaimer) -- and per the baseline benchmark's finding that arrayFold's `if`/`multiIf`
-- never short-circuit, whether any of these synthetic instructions would
-- "actually" halt doesn't change the node count *the fold itself* evaluates
-- per step, so this reuses the baseline benchmark's fixture generator largely unmodified.
--
-- That property does NOT extend to the *e2e* harness, and an earlier
-- version of this file's own comment claimed it did -- wrong, found by
-- actually checking `batch_out.retired` rather than trusting the theory.
-- the baseline benchmark's mix used raw small `imm` values as load/store addresses, sound
-- only because the baseline benchmark's fold had no bounds checking at all. #23 adds
-- BAD_ADDR: an address that isn't `imm` alone but `regs[rs1] + imm`, and
-- with `regs[rs1]` starting at 0, an unadjusted `imm` in [0, 4096) is never
-- inside RAM (`[0x8000_0000, ...)`). The very first load/store in the
-- stream halted BAD_ADDR, and every "batch" after that re-hit the same
-- frozen halt instantly -- e2e wall-clock still measured *something*, but
-- not 600,000 instructions of work, invalidating the throughput number
-- computed from it. Fixed below: load/store forces `rs1 = 0` and a
-- `imm` that is unconditionally `RAM_BASE`-relative, past the text window
-- (so stores can't SELF_MODIFY either) and 4-byte aligned (so no width
-- choice can MISALIGNED). `target` (branches/jal) was already safe -- a
-- byte address derived from a word index inside the text window, always
-- 4-aligned.
--
-- jalr (id=27) needed the same fix, found the same way (checking
-- batch_out, not assuming): its target is `regs[rs1] + imm`, computed live,
-- and `regs[rs1]` accumulates essentially arbitrary values from upstream
-- ALU arms over the course of a run -- an unconstrained register value's
-- bit 1 is a coin flip, so jalr misaligned roughly half the time it was
-- taken. Same treatment as load/store: force rs1 = 0 and give jalr a
-- deterministically-aligned text-window `imm`, matching `tgt`'s pattern
-- (not realistic RISC-V codegen for jalr specifically, but this harness
-- doesn't need decode realism -- only "doesn't spuriously halt").

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
    -- Load/store imm is RAM_BASE + an offset past the 2 MiB text window,
    -- into the remaining ~22 MiB of RAM, always a multiple of 4 (safe for
    -- byte/half/word alike) -- rs1 forced to 0 above, so this is the whole
    -- address, unconditionally in-bounds and never self-modifying. jalr's
    -- imm (rs1 also forced to 0) is instead a text-window byte address,
    -- matching `tgt`'s pattern, so a taken jalr always jumps somewhere
    -- decoded rather than off into the data region.
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

TRUNCATE TABLE {{DB}}.batch_commit;
TRUNCATE TABLE {{DB}}.cpu_state;
TRUNCATE TABLE {{DB}}.console_out;
TRUNCATE TABLE {{DB}}.input_queue;
