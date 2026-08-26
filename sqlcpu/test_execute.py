#!/usr/bin/env python3
"""Correctness test for sqlcpu/execute.py — issue #19.

Cross-checks the SQL expressions execute.py generates against an
independent, plain-Python RV32I oracle (deliberately not sharing any code
with execute.py, so a bug in one is unlikely to be mirrored in the other),
run over sqlcpu/fixtures/decode_vectors.tsv — the same 51 hand-encoded
instructions sqlcpu/test_decode.sh already proves decode.sql handles
correctly, including its 8 M-extension rows (id 10..17).

Also exercises cases the fixture file doesn't happen to hit, since it was
written before some of these were in scope:
  * Writing to x0. SPEC §1 requires the write be discarded, not merely be
    harmless — tested directly against execute.py's regs_write().
  * Eager MISALIGNED (SPEC §1, agreed with refemu on #37): the fixture's
    branch/jal offsets are all multiples of 4 by construction, so none of
    them exercise a misaligned target at all. misaligned_vectors() adds
    dedicated jal/jalr/taken-branch/not-taken-branch cases mirroring
    refemu's PR #51 test suite.
  * M-extension edge cases (issue #20/#12): division by zero, INT_MIN/-1
    signed overflow, and mulhsu's signed/unsigned operand asymmetry. The
    fixture's mul/mulh/.../remu rows use rs1=1, rs2=2 (values 100, 200) —
    neither negative nor zero — so none of these ever come up there.
    m_extension_edge_case_vectors() patches specific registers via
    build_query()'s per-row register-override support to force them.

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


def _trunc_div(a, b):
    """C-style division truncated toward zero -- matches refemu's cpu.py
    (issue #12) and RISC-V div/rem, not Python's floor-dividing `//`."""
    q = abs(a) // abs(b)
    return -q if (a < 0) != (b < 0) else q


def _trunc_rem(a, b):
    return a - _trunc_div(a, b) * b


def oracle(pc, id_, rd, rs1, rs2, imm, tgt, mk, sg, regs=None):
    """Independent RV32I reference: same inputs execute.py's expressions take.

    `pc` and `tgt` are both byte addresses here, matching execute.py's
    current convention (fixed post-#37: an earlier word-indexed `pcidx`
    convention silently discarded a set bit 1 -- a target that's 2-byte
    but not 4-byte aligned -- instead of leaving it checkable as
    MISALIGNED).

    `regs`: the register file to read/write against, default REGS -- M-ext
    edge cases (issue #20) need specific registers (zero, INT_MIN, -1, a
    top-bit-set unsigned value) REGS's plain 100/200/300... sequence never
    produces.
    """
    if regs is None:
        regs = REGS
    def read_(r):
        return 0 if r == 0 else regs[r - 1]
    A = read_(rs1)
    rs2_value = read_(rs2)  # raw, NOT +imm -- see store_value's docstring
    B = u32(rs2_value + imm)

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
    # M-extension (issue #20): matches refemu's cpu.py (#12) edge cases --
    # no trap on div-by-zero or INT_MIN/-1 overflow, mulhsu is signed rs1 x
    # UNSIGNED rs2 (B stays unsigned, not sign-extended via s32()).
    elif id_ == 10: result = u32(A * B)
    elif id_ == 11: result = u32((s32(A) * s32(B)) >> 32)
    elif id_ == 12: result = u32((s32(A) * B) >> 32)
    elif id_ == 13: result = u32((A * B) >> 32)
    elif id_ == 14:
        if B == 0: result = 0xFFFFFFFF
        elif s32(A) == -0x80000000 and s32(B) == -1: result = 0x80000000
        else: result = u32(_trunc_div(s32(A), s32(B)))
    elif id_ == 15: result = 0xFFFFFFFF if B == 0 else A // B
    elif id_ == 16:
        if B == 0: result = A
        elif s32(A) == -0x80000000 and s32(B) == -1: result = 0
        else: result = u32(_trunc_rem(s32(A), s32(B)))
    elif id_ == 17: result = A if B == 0 else A % B
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

    new_regs = list(regs)
    if rd != 0 and not misaligned:
        new_regs[rd - 1] = result

    is_st = 1 if id_ == 19 else 0
    addr = u32(A + imm)
    st_addr = addr >> 2 if is_st else None
    st_val = None
    if is_st:
        shift = 8 * (addr & 3)
        # rs2_value, NOT B: B has the store's address-offset imm folded in
        # (ADR-0002's collapse, correct for computing the address from A),
        # which is not part of the value being stored.
        st_val = u32((STORE_WORD & u32(~(mk << shift))) | ((rs2_value & mk) << shift))

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
            rows.append((int(wa), id_, int(rd), int(rs1), int(rs2), int(imm),
                         int(tgt) if tgt else 0, int(mk), int(sg), note))
    return rows


M_EXT_REG_OVERRIDES = {3: 0, 4: 0x80000000, 5: 0xFFFFFFFF, 6: 0xFFFFFF9C, 7: 0x80000001}
# x3=0 (div-by-zero), x4=INT_MIN, x5=-1 (overflow), x6=-100 (mulhsu's signed
# operand), x7=a value with the top bit set (mulhsu's UNSIGNED operand --
# chosen so treating it as signed instead would flip the product's sign).


def m_extension_edge_case_vectors():
    """M-extension edge cases (issue #20/#12) the fixture's happy-path mul/
    mulh/.../remu rows (rs1=1, rs2=2 -> 100, 200, neither negative nor zero)
    don't exercise at all: division by zero, INT_MIN/-1 signed overflow, and
    mulhsu's signed-rs1/unsigned-rs2 asymmetry. Uses M_EXT_REG_OVERRIDES via
    build_query's per-row regs support -- REGS's plain 100/200/300...
    sequence has no zero, negative, or top-bit-set values to test with."""
    wa = 3000
    rows = [
        (wa,     14, 5, 1, 3, 0, 0, 0, 0, "div by zero -> all-ones, no trap"),      # rs2=x3=0
        (wa + 1, 15, 5, 1, 3, 0, 0, 0, 0, "divu by zero -> all-ones, no trap"),
        (wa + 2, 16, 5, 1, 3, 0, 0, 0, 0, "rem by zero -> dividend, no trap"),
        (wa + 3, 17, 5, 1, 3, 0, 0, 0, 0, "remu by zero -> dividend, no trap"),
        (wa + 4, 14, 5, 4, 5, 0, 0, 0, 0, "div INT_MIN/-1 overflow -> INT_MIN"),    # x4=INT_MIN, x5=-1
        (wa + 5, 16, 5, 4, 5, 0, 0, 0, 0, "rem INT_MIN/-1 overflow -> 0"),
        (wa + 6, 12, 5, 6, 7, 0, 0, 0, 0, "mulhsu: signed rs1 x UNSIGNED rs2"),     # x6 negative, x7 top-bit set
    ]
    return [row + (M_EXT_REG_OVERRIDES,) for row in rows]


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


def _row_parts(rows):
    """One SELECT-block string per row (no UNION ALL / FORMAT -- that's
    build_queries()'s job, so it can batch these into AST-size-limited
    chunks instead of one query per test run)."""
    a_expr = ex.operand_a()
    b_expr = ex.operand_b()
    result_expr = ex.alu_result("loaded_word", "addr_load", pc="pc")
    # Bound once per row via the WITH clause below (`misaligned`) and passed
    # through to all four -- see execute.py's `misaligned=` parameter
    # docstring: four independent copies of is_misaligned() per row is what
    # first blew ClickHouse's AST-size limit here.
    next_expr = ex.next_pc(pc="pc", misaligned="misaligned")
    newregs_expr = ex.regs_write(ex.rd_or_suppressed(misaligned="misaligned"), result_expr)
    isstore_expr = ex.is_store()
    staddr_expr = ex.store_word_addr()
    stval_expr = ex.store_value("loaded_word_store", "addr_store", rs2_value="rs2_value")
    halted_expr = ex.halted(misaligned="misaligned")
    haltreason_expr = ex.halt_reason(misaligned="misaligned")

    parts = []
    for row in rows:
        # 10-tuple: default REGS. 11-tuple: an extra {reg_num: value}
        # override dict (see M_EXT_REG_OVERRIDES) -- REGS's plain
        # 100/200/300... sequence can't exercise zero/negative/overflow
        # edge cases, so a few dedicated vectors need a patched register file.
        if len(row) == 11:
            wa, id_, rd, rs1, rs2, imm, tgt, mk, sg, note, overrides = row
        else:
            wa, id_, rd, rs1, rs2, imm, tgt, mk, sg, note = row
            overrides = {}
        regs = list(REGS)
        for reg_num, value in overrides.items():
            regs[reg_num - 1] = value
        regs_sql = "[" + ",".join(str(v) for v in regs) + "]"

        pc = wa * 4
        exp_next, exp_regs, exp_isst, exp_staddr, exp_stval, exp_halted, exp_reason = oracle(
            pc, id_, rd, rs1, rs2, imm, tgt, mk, sg, regs=regs)
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
        ({ex.reg_read('rs2')}) AS rs2_value,
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
    default_regs_sql = "[" + ",".join(str(v) for v in REGS) + "]"
    x0_read = ex.reg_read("0")
    x0_write = ex.regs_write("0", "999999")
    parts.append(f"""SELECT
    -1 AS word_addr, 'x0 read is always zero' AS note,
    ({x0_read}) = 0 AS next_ok, 1 AS regs_ok, 1 AS isstore_ok, 1 AS staddr_ok,
    1 AS stval_ok, 1 AS halted_ok, 1 AS haltreason_ok
FROM (SELECT {default_regs_sql} AS regs)""")
    parts.append(f"""SELECT
    -2 AS word_addr, 'x0 write is discarded' AS note,
    1 AS next_ok, ({x0_write}) = regs AS regs_ok, 1 AS isstore_ok, 1 AS staddr_ok,
    1 AS stval_ok, 1 AS halted_ok, 1 AS haltreason_ok
FROM (SELECT {default_regs_sql} AS regs)""")
    return parts


def build_queries(rows, batch_size=8):
    """A list of complete queries, each a UNION ALL of at most `batch_size`
    row-checks. M-extension's wider alu_result() plus the shared
    `misaligned` condition pushed a single all-rows UNION ALL past
    ClickHouse's AST-size limit (50,000 nodes) well before all ~65 rows
    were included -- batching keeps each query comfortably under that
    regardless of how large any individual row's expressions grow."""
    parts = _row_parts(rows)
    queries = []
    for i in range(0, len(parts), batch_size):
        batch = parts[i:i + batch_size]
        queries.append("\nUNION ALL\n".join(batch) + "\nORDER BY word_addr\nFORMAT TSVWithNames")
    return queries


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--client", default="clickhouse-client")
    ap.add_argument("--fixture", default="sqlcpu/fixtures/decode_vectors.tsv")
    args = ap.parse_args()

    rows = load_vectors(args.fixture) + misaligned_vectors() + m_extension_edge_case_vectors()
    queries = build_queries(rows)

    client_cmd = args.client.split() + ["--host", args.host, "--port", str(args.port), "--user", args.user]
    if args.password:
        client_cmd += ["--password", args.password]

    total = 0
    fail = 0
    for query in queries:
        result = subprocess.run(client_cmd, input=query, text=True, capture_output=True)
        if result.returncode != 0:
            print(result.stderr, file=sys.stderr)
            return result.returncode

        lines = result.stdout.strip("\n").split("\n")
        header, data = lines[0].split("\t"), lines[1:]
        total += len(data)
        for line in data:
            cols = dict(zip(header, line.split("\t")))
            if not all(cols[c] == "1" for c in ("next_ok", "regs_ok", "isstore_ok",
                                                  "staddr_ok", "stval_ok", "halted_ok",
                                                  "haltreason_ok")):
                fail += 1
                print(f"::error::execute mismatch on word_addr={cols['word_addr']} ({cols['note']}): {cols}",
                      file=sys.stderr)

    if fail:
        print(f"execute.py: {total - fail}/{total} checks passed, {fail} FAILED", file=sys.stderr)
        return 1
    print(f"execute.py: all {total} checks passed across {len(queries)} batched queries "
          f"({len(rows)} instruction rows (fixture + dedicated MISALIGNED/M-extension edge "
          f"cases) + 2 dedicated x0 checks)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
