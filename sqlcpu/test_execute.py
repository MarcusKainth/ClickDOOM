#!/usr/bin/env python3
"""Correctness test for sqlcpu/execute.py — issue #19.

Cross-checks the SQL expressions execute.py generates against an
independent, plain-Python RV32I oracle (deliberately not sharing any code
with execute.py, so a bug in one is unlikely to be mirrored in the other),
run over sqlcpu/fixtures/decode_vectors.tsv — the same 51 hand-encoded
instructions sqlcpu/test_decode.sh already proves decode.sql handles
correctly. M-extension rows (decoded.id 10..17) are skipped: execute.py
leaves that arm a placeholder pending issue #20.

Also exercises two cases the fixture file doesn't happen to hit, since it
was written before either was in scope:
  * Writing to x0. SPEC §1 requires the write be discarded, not merely be
    harmless — tested directly against execute.py's regs_write().
  * Eager MISALIGNED (SPEC §1, agreed with refemu on #37): the fixture's
    branch/jal offsets are all multiples of 4 by construction, so none of
    them exercise a misaligned target at all. misaligned_vectors() adds
    dedicated jal/jalr/taken-branch/not-taken-branch cases mirroring
    refemu's PR #51 test suite.

This does not run inside arrayFold — each row is evaluated as an
independent single-row SELECT with its own literal register file, which is
sufficient to prove the per-instruction expression correct without needing
executor's batch loop (#23) to exist yet. Usage:
    test_execute.py --host localhost --port 9000 --password clickdoom
"""
import argparse
import subprocess
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import execute as ex  # noqa: E402

REGS = [(i + 1) * 100 for i in range(31)]  # x1..x31 = 100..3100
LOAD_WORD = 0xDEADBEEF
STORE_WORD = 0x12345678


def u32(x):
    return x & 0xFFFFFFFF


def s32(x):
    x &= 0xFFFFFFFF
    return x - 0x100000000 if x & 0x80000000 else x


def read(r):
    return 0 if r == 0 else REGS[r - 1]


def oracle(pc, id_, rd, rs1, rs2, imm, tgt, mk, sg):
    """Independent RV32I reference: same inputs execute.py's expressions take.

    `pc` and `tgt` are both byte addresses here, matching execute.py's
    current convention (fixed post-#37: an earlier word-indexed `pcidx`
    convention silently discarded a set bit 1 -- a target that's 2-byte
    but not 4-byte aligned -- instead of leaving it checkable as
    MISALIGNED).
    """
    A = read(rs1)
    B = u32(read(rs2) + imm)

    if id_ == 18:
        addr = u32(A + imm)
        shift = 8 * (addr & 3)
        extracted = (LOAD_WORD >> shift) & mk
        sign_bit = (mk >> 1) + 1
        result = u32(extracted - (mk + 1)) if (sg and (extracted & sign_bit)) else extracted
    elif id_ == 0: result = u32(A + B)
    elif id_ == 1: result = u32(A - B)
    elif id_ == 2: result = u32(A << (B & 31))
    elif id_ == 3: result = 1 if s32(A) < s32(B) else 0
    elif id_ == 4: result = 1 if A < B else 0
    elif id_ == 5: result = A ^ B
    elif id_ == 6: result = A >> (B & 31)
    elif id_ == 7: result = u32(s32(A) >> (B & 31))
    elif id_ == 8: result = A | B
    elif id_ == 9: result = A & B
    else: result = u32(pc + 4)  # jal/jalr link value

    jalr_target = u32(A + imm) & 0xFFFFFFFE
    taken_target = {20: A == B, 21: A != B, 22: s32(A) < s32(B), 23: s32(A) >= s32(B),
                     24: A < B, 25: A >= B, 26: True, 27: True}.get(id_)
    target = jalr_target if id_ == 27 else tgt
    # Eager MISALIGNED (SPEC §1, #37): a TAKEN branch/jal/jalr whose target
    # isn't 4-byte aligned halts at ITS OWN pc, updating neither pc nor rd.
    misaligned = bool(taken_target) and (target & 3) != 0

    if misaligned:
        nxt = pc
    elif id_ == 20: nxt = tgt if A == B else pc + 4
    elif id_ == 21: nxt = tgt if A != B else pc + 4
    elif id_ == 22: nxt = tgt if s32(A) < s32(B) else pc + 4
    elif id_ == 23: nxt = tgt if s32(A) >= s32(B) else pc + 4
    elif id_ == 24: nxt = tgt if A < B else pc + 4
    elif id_ == 25: nxt = tgt if A >= B else pc + 4
    elif id_ == 26: nxt = tgt
    elif id_ == 27: nxt = jalr_target
    else: nxt = pc + 4

    new_regs = list(REGS)
    if rd != 0 and not misaligned:
        new_regs[rd - 1] = result

    is_st = 1 if id_ == 19 else 0
    addr = u32(A + imm)
    st_addr = addr >> 2 if is_st else None
    st_val = None
    if is_st:
        shift = 8 * (addr & 3)
        st_val = u32((STORE_WORD & u32(~(mk << shift))) | ((B & mk) << shift))

    halted = 1 if (28 <= id_ <= 31 or misaligned) else 0
    reason = {28: "ECALL", 29: "EBREAK", 30: "CSR", 31: "ILLEGAL_INSN"}.get(id_, "")
    if misaligned:
        reason = "MISALIGNED"
    return u32(nxt), new_regs, is_st, st_addr, st_val, halted, reason


def load_vectors(path):
    rows = []
    with open(path) as f:
        for line in f:
            wa, _word, id_, rd, rs1, rs2, imm, tgt, mk, sg, note = line.rstrip("\n").split("\t")
            id_ = int(id_)
            if 10 <= id_ <= 17:
                continue  # M-extension: issue #20
            rows.append((int(wa), id_, int(rd), int(rs1), int(rs2), int(imm),
                         int(tgt) if tgt else 0, int(mk), int(sg), note))
    return rows


def misaligned_vectors():
    """Dedicated MISALIGNED (SPEC §1, eager per #37) test rows -- the decode
    fixture's branch/jal offsets are all multiples of 4 by construction, so
    none of them exercise this at all. Mirrors refemu's PR #51 test cases:
    jal, jalr, a taken branch (all halt on their own pc), and a not-taken
    branch whose target would have been bad if taken (must NOT halt)."""
    wa = 2000
    rows = [
        # jal x5, target = pc+2 (misaligned)
        (wa, 26, 5, 0, 0, 0, wa * 4 + 2, 0, 0, "jal to misaligned target"),
        # jalr x6, x1, 2  -- x1 = REGS[0] = 100; (100+2) has bit1 set
        (wa + 1, 27, 6, 1, 0, 2, 0, 0, 0, "jalr to misaligned target"),
        # beq x1, x1, taken (trivially equal), target = pc+2 (misaligned)
        (wa + 2, 20, 0, 1, 1, 0, (wa + 2) * 4 + 2, 0, 0, "taken beq to misaligned target"),
        # bne x1, x2, taken (100 != 200), target = pc+2 (misaligned)
        (wa + 3, 21, 0, 1, 2, 0, (wa + 3) * 4 + 2, 0, 0, "taken bne to misaligned target"),
        # beq x1, x2, NOT taken (100 != 200) -- target would be misaligned
        # if taken, but must not fault since the branch isn't taken.
        (wa + 4, 20, 0, 1, 2, 0, (wa + 4) * 4 + 2, 0, 0, "untaken beq, bad target never checked"),
    ]
    return rows


def build_query(rows):
    a_expr = ex.operand_a()
    b_expr = ex.operand_b()
    result_expr = ex.alu_result("loaded_word", "addr_load", pc="pc")
    # Bound once per row via the WITH clause below (`misaligned`) and passed
    # through to all four -- see execute.py's `misaligned=` parameter
    # docstring: four independent copies of is_misaligned() per row is what
    # blew ClickHouse's AST-size limit across ~56 UNION ALL'd test rows.
    next_expr = ex.next_pc(pc="pc", misaligned="misaligned")
    newregs_expr = ex.regs_write(ex.rd_or_suppressed(misaligned="misaligned"), result_expr)
    isstore_expr = ex.is_store()
    staddr_expr = ex.store_word_addr()
    stval_expr = ex.store_value("loaded_word_store", "addr_store")
    halted_expr = ex.halted(misaligned="misaligned")
    haltreason_expr = ex.halt_reason(misaligned="misaligned")
    regs_sql = "[" + ",".join(str(v) for v in REGS) + "]"

    parts = []
    for wa, id_, rd, rs1, rs2, imm, tgt, mk, sg, note in rows:
        pc = wa * 4
        exp_next, exp_regs, exp_isst, exp_staddr, exp_stval, exp_halted, exp_reason = oracle(
            pc, id_, rd, rs1, rs2, imm, tgt, mk, sg)
        exp_regs_sql = "[" + ",".join(str(v) for v in exp_regs) + "]"
        # store address/value are only meaningful (and only oracle-checked) on
        # store rows -- on every other row the same expressions still compute
        # *something* deterministic (an unused byte address derived from
        # A+imm), which isn't wrong, just irrelevant, so don't assert it.
        staddr_check = f"staddr_val = {exp_staddr}" if exp_isst else "1"
        stval_check = f"stval_val = {exp_stval}" if exp_isst else "1"
        note_sql = note.replace("'", "''")
        parts.append(f"""SELECT
    {wa} AS word_addr,
    '{note_sql}' AS note,
    next_val = {exp_next} AS next_ok,
    regs_val = {exp_regs_sql} AS regs_ok,
    isstore_val = {exp_isst} AS isstore_ok,
    ({staddr_check}) AS staddr_ok,
    ({stval_check}) AS stval_ok,
    halted_val = {exp_halted} AS halted_ok,
    haltreason_val = '{exp_reason}' AS haltreason_ok
FROM
(
    WITH
        toUInt8({rs1}) AS rs1, toUInt8({rs2}) AS rs2, toUInt32({imm}) AS imm,
        toUInt8({id_}) AS id, toUInt8({rd}) AS rd, toUInt32({tgt}) AS tgt,
        toUInt32({mk}) AS mk, toUInt8({sg}) AS sg, toUInt32({pc}) AS pc,
        {regs_sql} AS regs,
        toUInt32({LOAD_WORD}) AS loaded_word, toUInt32({STORE_WORD}) AS loaded_word_store,
        ({a_expr}) AS A, ({b_expr}) AS B,
        toUInt32(A + imm) AS addr_load, toUInt32(A + imm) AS addr_store,
        ({ex.is_misaligned()}) AS misaligned
    SELECT
        ({next_expr}) AS next_val, ({newregs_expr}) AS regs_val,
        ({isstore_expr}) AS isstore_val, ({staddr_expr}) AS staddr_val,
        ({stval_expr}) AS stval_val, ({halted_expr}) AS halted_val,
        ({haltreason_expr}) AS haltreason_val
)""")

    # x0 is never exercised by the fixture rows (none of the 50 vectors target
    # rd=0 via a real ALU op), so it gets its own dedicated pair of checks:
    # read-as-zero, and write-discarded (array unchanged), straight from
    # execute.py's own helpers rather than the fixture-driven path above.
    x0_read = ex.reg_read("0")
    x0_write = ex.regs_write("0", "999999")
    parts.append(f"""SELECT
    -1 AS word_addr, 'x0 read is always zero' AS note,
    ({x0_read}) = 0 AS next_ok, 1 AS regs_ok, 1 AS isstore_ok, 1 AS staddr_ok,
    1 AS stval_ok, 1 AS halted_ok, 1 AS haltreason_ok
FROM (SELECT {regs_sql} AS regs)""")
    parts.append(f"""SELECT
    -2 AS word_addr, 'x0 write is discarded' AS note,
    1 AS next_ok, ({x0_write}) = regs AS regs_ok, 1 AS isstore_ok, 1 AS staddr_ok,
    1 AS stval_ok, 1 AS halted_ok, 1 AS haltreason_ok
FROM (SELECT {regs_sql} AS regs)""")

    return "\nUNION ALL\n".join(parts) + "\nORDER BY word_addr\nFORMAT TSVWithNames"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--client", default="clickhouse-client")
    ap.add_argument("--fixture", default="sqlcpu/fixtures/decode_vectors.tsv")
    args = ap.parse_args()

    rows = load_vectors(args.fixture) + misaligned_vectors()
    query = build_query(rows)

    client_cmd = args.client.split() + ["--host", args.host, "--port", str(args.port), "--user", args.user]
    if args.password:
        client_cmd += ["--password", args.password]

    result = subprocess.run(client_cmd, input=query, text=True, capture_output=True)
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        return result.returncode

    lines = result.stdout.strip("\n").split("\n")
    header, data = lines[0].split("\t"), lines[1:]
    fail = 0
    for line in data:
        cols = dict(zip(header, line.split("\t")))
        if not all(cols[c] == "1" for c in ("next_ok", "regs_ok", "isstore_ok",
                                              "staddr_ok", "stval_ok", "halted_ok",
                                              "haltreason_ok")):
            fail += 1
            print(f"::error::execute mismatch on word_addr={cols['word_addr']} ({cols['note']}): {cols}",
                  file=sys.stderr)

    total = len(data)
    if fail:
        print(f"execute.py: {total - fail}/{total} checks passed, {fail} FAILED", file=sys.stderr)
        return 1
    print(f"execute.py: all {total} checks passed ({len(rows)} instruction rows "
          f"(fixture + dedicated MISALIGNED cases) + 2 dedicated x0 checks)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
