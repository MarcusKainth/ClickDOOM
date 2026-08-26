-- sqlcpu/decode.sql — build clickdoom.decoded from clickdoom.ram (SPEC §1,
-- ADR-0002, issue #18). PURITY.md's purity-critical statement: this decode
-- happens entirely inside ClickHouse. The driver's only involvement is
-- loading ROM bytes into `ram` beforehand (sqlcpu/load_rom.py, housekeeping
-- that computes nothing) and invoking this file; nothing here nor there
-- decodes a RISC-V instruction outside SQL.
--
-- Scope: the text region only, [text_start_word, text_end_word) in the same
-- word_addr domain as `ram` (byte address >> 2) — the bounds ROM's
-- manifest.json (SPEC §4) declares. Query parameters, not string
-- substitution, carry them in: run as
--   clickhouse-client --param_text_start_word=N --param_text_end_word=M \
--     --database clickdoom --multiquery < sqlcpu/decode.sql
-- Re-running against the same ram contents is bit-identical: every column
-- is a pure function of (word_addr, value), nothing here reads icount,
-- wall time, or any other batch-varying state. Reads `ram FINAL` — without
-- it, an unmerged duplicate version of a word_addr (from re-loading the
-- same ROM, or from stores accumulating between merges) produces duplicate
-- decoded rows for that word, not a wrong-but-single one, so this isn't
-- optional the way it can be for a one-off read.
--
-- Column semantics, `id` numbering and the R/I-type + lui/auipc/jal/jalr
-- collapses are documented above `CREATE TABLE clickdoom.decoded` in
-- sqlcpu/schema.sql — this file is the query that fills it, not a second
-- copy of that design note.
--
-- Two sentinel ids outside the 0..27 dispatch range, for opcodes SPEC §1
-- requires a fatal halt on, so execute (#19) can tell them apart without
-- re-decoding the raw word:
--   254  opcode 0x73 (ecall / ebreak / any CSR instruction)
--   255  anything else unrecognized (bad opcode, reserved funct3/funct7,
--        or a 16-bit compressed encoding — bits[1:0] != 11 never matches
--        any named opcode below, so it falls through here too)
-- FENCE / FENCE.I (opcode 0x0F) is not a sentinel: RV32IM here is a single
-- in-order hart with no atomics, so FENCE has nothing to order and decodes
-- as a true no-op — the `add` arm (id 0) with rd forced to 0.
--
-- TRUNCATE first: decode always rebuilds the whole table from the current
-- text region rather than appending, so re-running this file against
-- unchanged `ram` contents leaves `decoded` bit-identical rather than
-- doubled.
TRUNCATE TABLE IF EXISTS clickdoom.decoded;

INSERT INTO clickdoom.decoded
WITH fields AS
(
    SELECT
        word_addr,
        value                                     AS w,
        bitAnd(value, 127)                        AS op,
        bitAnd(bitShiftRight(value, 7), 31)       AS rd_f,
        bitAnd(bitShiftRight(value, 12), 7)       AS f3,
        bitAnd(bitShiftRight(value, 15), 31)      AS rs1_f,
        bitAnd(bitShiftRight(value, 20), 31)      AS rs2_f,   -- also I-type shamt[4:0]
        bitShiftRight(value, 25)                  AS f7,
        toUInt32(toInt32(bitShiftRight(value, 20))
            - if(bitAnd(value, 2147483648) != 0, 4096, 0))                        AS iimm,
        toUInt32(toInt32(bitOr(bitShiftRight(bitAnd(value, 4294967295), 25) * 32,
                                bitAnd(bitShiftRight(value, 7), 31)))
            - if(bitAnd(value, 2147483648) != 0, 4096, 0))                        AS simm,
        toUInt32(bitAnd(value, 4294963200))                                       AS uimm,
        toUInt32(toInt32(bitAnd(bitShiftRight(value, 7), 30)
                          + bitAnd(bitShiftRight(value, 20), 2016)
                          + bitShiftLeft(bitAnd(bitShiftRight(value, 7), 1), 11))
            - if(bitAnd(value, 2147483648) != 0, 4096, 0))                        AS bimm,
        toUInt32(toInt32(bitAnd(bitShiftRight(value, 20), 2046)
                          + bitShiftLeft(bitAnd(bitShiftRight(value, 20), 1), 11)
                          + bitAnd(value, 1044480))
            - if(bitAnd(value, 2147483648) != 0, 1048576, 0))                     AS jimm,
        toUInt32(word_addr * 4)                                                   AS pc
    FROM clickdoom.ram FINAL
    WHERE word_addr >= {text_start_word:UInt32} AND word_addr < {text_end_word:UInt32}
)
SELECT
    '0.1.0' AS spec_version,
    word_addr,
    multiIf(
        op = 55, 0,                              -- lui  -> add arm, rs1=rs2=0, imm=uimm
        op = 23, 0,                              -- auipc-> add arm, rs1=rs2=0, imm=pc+uimm
        op = 111, 26,                            -- jal
        op = 103 AND f3 = 0, 27,                 -- jalr
        op = 103, 255,
        op = 99 AND f3 = 0, 20,                  -- beq
        op = 99 AND f3 = 1, 21,                  -- bne
        op = 99 AND f3 = 4, 22,                  -- blt
        op = 99 AND f3 = 5, 23,                  -- bge
        op = 99 AND f3 = 6, 24,                  -- bltu
        op = 99 AND f3 = 7, 25,                  -- bgeu
        op = 99, 255,
        op = 3 AND f3 IN (0, 1, 2, 4, 5), 18,    -- load
        op = 3, 255,
        op = 35 AND f3 IN (0, 1, 2), 19,         -- store
        op = 35, 255,
        op = 15, 0,                              -- fence/fence.i -> no-op (rd forced 0 below)
        op = 19 AND f3 = 0, 0,                   -- addi
        op = 19 AND f3 = 1 AND f7 = 0, 2,        -- slli
        op = 19 AND f3 = 2, 3,                   -- slti
        op = 19 AND f3 = 3, 4,                   -- sltiu
        op = 19 AND f3 = 4, 5,                   -- xori
        op = 19 AND f3 = 5 AND f7 = 0, 6,        -- srli
        op = 19 AND f3 = 5 AND f7 = 32, 7,       -- srai
        op = 19 AND f3 = 6, 8,                   -- ori
        op = 19 AND f3 = 7, 9,                   -- andi
        op = 19, 255,
        op = 51 AND f7 = 1 AND f3 = 0, 10,       -- mul
        op = 51 AND f7 = 1 AND f3 = 1, 11,       -- mulh
        op = 51 AND f7 = 1 AND f3 = 2, 12,       -- mulhsu
        op = 51 AND f7 = 1 AND f3 = 3, 13,       -- mulhu
        op = 51 AND f7 = 1 AND f3 = 4, 14,       -- div
        op = 51 AND f7 = 1 AND f3 = 5, 15,       -- divu
        op = 51 AND f7 = 1 AND f3 = 6, 16,       -- rem
        op = 51 AND f7 = 1 AND f3 = 7, 17,       -- remu
        op = 51 AND f7 = 0  AND f3 = 0, 0,       -- add
        op = 51 AND f7 = 32 AND f3 = 0, 1,       -- sub
        op = 51 AND f7 = 0  AND f3 = 1, 2,       -- sll
        op = 51 AND f7 = 0  AND f3 = 2, 3,       -- slt
        op = 51 AND f7 = 0  AND f3 = 3, 4,       -- sltu
        op = 51 AND f7 = 0  AND f3 = 4, 5,       -- xor
        op = 51 AND f7 = 0  AND f3 = 5, 6,       -- srl
        op = 51 AND f7 = 32 AND f3 = 5, 7,       -- sra
        op = 51 AND f7 = 0  AND f3 = 6, 8,       -- or
        op = 51 AND f7 = 0  AND f3 = 7, 9,       -- and
        op = 51, 255,
        op = 115, 254,                           -- ecall/ebreak/csr -> fatal halt (SPEC §1)
        255                                       -- unimplemented/illegal opcode (SPEC §1)
    ) AS id,
    multiIf(op IN (55, 23, 111, 103, 3, 19, 51), rd_f, 0) AS rd,
    multiIf(op IN (19, 51, 3, 35, 99, 103), rs1_f, 0) AS rs1,
    multiIf(op IN (51, 35, 99), rs2_f, 0) AS rs2,
    multiIf(
        op = 55, uimm,                                            -- lui
        op = 23, toUInt32(pc + uimm),                              -- auipc, pc-relative constant
        op = 19 AND f3 IN (1, 5), toUInt32(rs2_f),                 -- slli/srli/srai: zero-extended shamt
        op = 19, iimm,                                             -- other op-imm
        op = 3, iimm,                                              -- load address offset
        op = 35, simm,                                             -- store address offset
        op = 103, iimm,                                            -- jalr address offset
        toUInt32(0)
    ) AS imm,
    multiIf(
        op = 111, bitShiftRight(toUInt32(pc + jimm), 2),           -- jal absolute target
        op = 99, bitShiftRight(toUInt32(pc + bimm), 2),            -- branch absolute target
        toUInt32(0)
    ) AS tgt,
    multiIf(
        op = 3 AND f3 IN (0, 4), 255,
        op = 3 AND f3 IN (1, 5), 65535,
        op = 3 AND f3 = 2, 4294967295,
        op = 35 AND f3 = 0, 255,
        op = 35 AND f3 = 1, 65535,
        op = 35 AND f3 = 2, 4294967295,
        toUInt32(0)
    ) AS mk,
    multiIf(op = 3 AND f3 IN (0, 1), 1, 0) AS sg
FROM fields
ORDER BY word_addr;
