#!/usr/bin/env python3
"""riscv-tests runner inside ClickHouse — sqlcpu workstream, issue #21.

Phase 1's milestone for this workstream: riscv-tests fully green *inside
ClickHouse*. Assembles decode.sql's `decoded` table with execute.py's
composable expressions into a single arrayFold, run to completion (halt or
a step cap) entirely within one query per test binary.

This is sqlcpu's OWN fold assembly, not executor's production one (#23/#48)
-- CLAUDE.md's ownership table gives "riscv-tests harness running inside
ClickHouse" to sqlcpu specifically, separate from the batch-commit loop
DOOM itself will run under. A riscv-tests case runs to completion (pass or
fail) in one shot, with no batch boundary and nothing to commit -- so
there's no write-log-flush-to-ram, no state-reload-from-cpu_state, none of
#23/#48's batching machinery. What's shared is exactly the CPU logic
itself: decode.sql and execute.py, unedited -- this file only adds the
accumulator/loop plumbing needed to drive them to a verdict.

Fixtures: refemu/tests/fixtures/riscv_tests/*.bin (48: 40 rv32ui + 8
rv32um), already vendored by refemu (issue #14) with the pass/fail
convention riscv-tests uses -- run to a clean `ecall` (RISC-V's exit
syscall, a7=93), pass iff a0 (x10) == 0, otherwise a0 encodes the failing
test number as (testnum << 1) | 1. fence_i and ma_data are deliberately
absent from that fixture set (self-modifying code and transparent
misaligned access are both fatal halts per SPEC §1/ADR-0002) -- nothing to
special-case here, they were never generated.

Decode field binding: id/rd/rs1/rs2/imm/tgt/mk/sg (from the decode arrays),
A, B, the eager MISALIGNED condition, the load/store byte address, its
word index, and the loaded word are each referenced multiple times across
alu_result()/next_pc()/is_store()/halted()/halt_reason(). Past the ~2-use
crossover the team lead's correction to Phase 0's RESULTS.md identifies,
`arrayMap(v -> ..., [expr])[1]` binds cheaper than recomputing (unlike a
query-level `WITH`, this idiom works inside an arrayFold lambda) -- bound
once each, nested, rather than left to recompute per reference.

Usage:
    run_riscv_tests.py --host localhost --port 9000 --password clickdoom
"""
import argparse
import struct
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import execute as ex  # noqa: E402

RAM_BASE = 0x80000000
RAM_BASE_WORD = RAM_BASE >> 2
DEFAULT_MAX_INSTRUCTIONS = 4096
# refemu's test_riscv_tests_suite.py caps at 200,000, but that's a Python
# interpreter that raises an exception (stops immediately) on halt.
# arrayFold has no early-exit: every element of range(K) is evaluated
# regardless of when the accumulator's `halted` flag was set (this step's
# own `if(acc.5 != 0, acc, ...)` just freezes the value, it doesn't shrink
# the fold), so K here is real wall-clock cost paid on every single test,
# not a rarely-hit safety ceiling. All 48 fixtures currently complete in
# well under 1,000 instructions (the longest, ld_st, retires 927 -- #72
# fixed a 1-off overcount here, this is the post-fix number) -- 4,096
# is generous headroom above that while keeping a genuine runaway-test
# case fast to notice instead of paying for 200,000 empty iterations to
# find out.
FIXTURES_DIR = Path(__file__).parent.parent / "refemu" / "tests" / "fixtures" / "riscv_tests"
REPO_ROOT = Path(__file__).parent.parent


def load_sql(words):
    """Reset ram/decoded, then load one test binary's words as literal
    VALUES (a few hundred to ~1,000 rows for these fixtures -- no staging
    table needed at this size). Byte reinterpretation only, per PURITY.md's
    driver allowance -- no decoding here, decode.sql (unmodified) does that
    in the next statement."""
    values = ",".join(f"({RAM_BASE_WORD + i},{w},0)" for i, w in enumerate(words))
    return f"TRUNCATE TABLE ram;\nINSERT INTO ram (word_addr, value, version) VALUES {values};\n"


def decode_sql(word_count):
    text_end_word = RAM_BASE_WORD + word_count
    raw = (Path(__file__).parent / "decode.sql").read_text()
    raw = raw.replace("{text_start_word:UInt32}", str(RAM_BASE_WORD))
    raw = raw.replace("{text_end_word:UInt32}", str(text_end_word))
    return raw


def step_expr():
    """The arrayFold step: `(acc, i) -> {step_expr()}`.

    Accumulator (7-tuple): pc, regs[31], wl_addr[], wl_val[], halted,
    halt_reason, icount. `wl_addr`/`wl_val` are word-addressed, write-log-
    first-then-RAM (ADR-0001), same as decode.sql/execute.py's own
    convention -- captured `RAM`/decode arrays below share that domain.
    """
    PC, REGS, WL_ADDR, WL_VAL = "acc.1", "acc.2", "acc.3", "acc.4"
    REL_IDX = f"(bitShiftRight(toUInt32({PC}), 2) - {RAM_BASE_WORD} + 1)"
    # DEC_T[i] = tuple(word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg) -- see
    # DECODE_ARRAYS's comment for why this is one combined capture rather
    # than one groupArray per column.
    d_expr = f"DEC_T[{REL_IDX}]"
    WORD_ADDR_AT, ID, RD, RS1, RS2, IMM, TGT, MK, SG = (f"d.{i}" for i in range(1, 10))

    a_expr = ex.reg_read(RS1, REGS)
    rs2v_expr = ex.reg_read(RS2, REGS)
    # `b` (regs[rs2] + imm) is the ALU/address operand (ADR-0002's R/I-type
    # collapse); `rs2v` (raw, no +imm) is what a store actually writes.
    # Passing `b` to store_value() instead of `rs2v` is a real, previously
    # committed bug (see execute.py's store_value() docstring) -- found here
    # by riscv-tests' sb/sh/sw/ld_st/st_ld fixtures, which a same-mistake
    # oracle in test_execute.py had been unable to catch.
    b_expr = f"toUInt32(rs2v + {IMM})"
    misaligned_expr = ex.is_misaligned(id_=ID, a="a", b="b", tgt=TGT, imm=IMM)
    addr_expr = f"toUInt32(a + {IMM})"
    wa_expr = "bitShiftRight(addr, 2)"
    lw_expr = (f"if(arrayLastIndex(z -> z = wa, {WL_ADDR}) > 0,"
               f" {WL_VAL}[arrayLastIndex(z -> z = wa, {WL_ADDR})],"
               f" RAM_T[wa - {RAM_BASE_WORD} + 1].2)")

    next_expr = ex.next_pc(id_=ID, a="a", b="b", tgt=TGT, imm=IMM, pc=PC, misaligned="misaligned")
    rd_susp = ex.rd_or_suppressed(id_=ID, a="a", b="b", tgt=TGT, imm=IMM, rd=RD, misaligned="misaligned")
    result_expr = ex.alu_result("lw", "addr", id_=ID, a="a", b="b", mk=MK, sg=SG, pc=PC)
    regs_write_expr = ex.regs_write(rd_susp, result_expr, REGS)
    is_store_expr = ex.is_store(ID)
    store_val_expr = ex.store_value("lw", "addr", rs2_value="rs2v", mk=MK)
    halted_expr = ex.halted(id_=ID, a="a", b="b", tgt=TGT, imm=IMM, misaligned="misaligned")
    haltreason_expr = ex.halt_reason(id_=ID, a="a", b="b", tgt=TGT, imm=IMM, misaligned="misaligned")

    final_body = (
        "tuple("
        f"{next_expr},"
        f"{regs_write_expr},"
        f"if({is_store_expr}, arrayPushBack({WL_ADDR}, wa), {WL_ADDR}),"
        f"if({is_store_expr}, arrayPushBack({WL_VAL}, {store_val_expr}), {WL_VAL}),"
        f"toUInt8({halted_expr}),"
        f"{haltreason_expr},"
        # A fatal-halt instruction does not retire (SPEC §1, ruled on #72
        # after this exact line -- unconditional acc.7+1 -- disagreed with
        # both refemu (Halted raises before CPU.icount+=1 runs) and
        # executor's fold (step_retires gates on HALT_CODE==0), a 1-off
        # miscount caught by diffing every riscv-tests fixture's icount
        # against refemu's oracle). Gated on the same halted_expr already
        # computed above for this step, so the instruction that first
        # raises the halt condition is the one step that does NOT bump
        # icount -- every step before it still does, exactly as before.
        f"toUInt32(acc.7 + if({halted_expr}, 0, 1)))"
    )

    nested = f"arrayMap(lw -> {final_body}, [{lw_expr}])[1]"
    nested = f"arrayMap(wa -> {nested}, [{wa_expr}])[1]"
    nested = f"arrayMap(addr -> {nested}, [{addr_expr}])[1]"
    nested = f"arrayMap(misaligned -> {nested}, [{misaligned_expr}])[1]"
    nested = f"arrayMap(b -> {nested}, [{b_expr}])[1]"
    nested = f"arrayMap(rs2v -> {nested}, [{rs2v_expr}])[1]"
    nested = f"arrayMap(a -> {nested}, [{a_expr}])[1]"
    nested = f"arrayMap(d -> {nested}, [{d_expr}])[1]"

    # A step after halted freezes the accumulator untouched (pc, regs,
    # halt_reason and icount all stay put) -- SPEC §1's halt record is
    # exactly the state as of the halting instruction, which itself does
    # not retire (see the icount comment above).
    return f"if(acc.5 != 0, acc, {nested})"


# CORRECTNESS-CRITICAL, found debugging this issue: a bare
# `(SELECT groupArray(col) FROM (SELECT col, word_addr FROM t ORDER BY
# word_addr))` -- the idiom Phase 0's bench and executor's fold.py also
# use, one such scalar subquery per captured column -- is NOT reliable in
# ClickHouse 26.3. `col`'s values can come back correctly *word_addr-count*
# and *type*, just silently reordered relative to word_addr, when
# `optimize_read_in_order` (on by default) decides it can stream `t`'s
# already-sort-key-ordered column store straight into groupArray without an
# explicit merge step keyed on the ORDER BY -- observed concretely on this
# fixture: `decoded.sg` at array position 22 read back 0 where the table
# itself (and every other captured column, including `id`/`mk`/`imm` at
# the very same position) agreed on 1. `SETTINGS optimize_read_in_order = 0`
# does fix it (verified), but a setting a future query can omit isn't a
# fix, it's a landmine -- the query text itself needs to make the bug
# structurally unreachable. `groupArray(tuple(word_addr, col1, col2, ...))`
# -- ONE aggregate call instead of N, each captured field carried in the
# same tuple as the sort key it was ordered by -- reads back correctly
# every time this was tested, and avoids the interaction that turned up
# other columns' handling as suspect too (RAM's own single-column capture
# tested fine in isolation, but was not trusted here given how easily
# `decoded`'s multi-column case looked fine until specifically checked).
# Flagged project-wide: PR #ADR-0001's whole array-capture idiom needs the
# same treatment wherever it's used (Phase 0's bench, executor's fold.py).
DECODE_ARRAYS = f"""
  (SELECT groupArray(tuple(word_addr, value)) FROM (SELECT word_addr, value FROM ram FINAL ORDER BY word_addr)) AS RAM_T,
  (SELECT groupArray(tuple(word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg))
     FROM (SELECT word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg FROM decoded ORDER BY word_addr)) AS DEC_T"""


def run_query(max_instructions):
    step = step_expr()
    init = ("tuple(toUInt32(%d), arrayResize(emptyArrayUInt32(), 31, toUInt32(0)),"
            " emptyArrayUInt32(), emptyArrayUInt32(), toUInt8(0), '', toUInt32(0))" % RAM_BASE)
    return f"""WITH{DECODE_ARRAYS}
SELECT r.1 AS pc, r.2 AS regs, r.5 AS halted, r.6 AS halt_reason, r.7 AS icount
FROM (SELECT arrayFold((acc, i) -> {step}, range({max_instructions}), {init}) AS r)
SETTINGS max_threads = 1
FORMAT TSVWithNames
"""


def run_one_fixture(ch, fixture: Path, max_instructions):
    data = fixture.read_bytes()
    if len(data) % 4 != 0:
        data = data + b"\x00" * (4 - len(data) % 4)
    words = struct.unpack(f"<{len(data) // 4}I", data)

    sql = load_sql(words) + decode_sql(len(words)) + run_query(max_instructions)
    result = ch(sql)
    lines = result.strip("\n").split("\n")
    header, row = lines[0].split("\t"), lines[1].split("\t")
    cols = dict(zip(header, row))

    halted = cols["halted"] == "1"
    halt_reason = cols["halt_reason"]
    icount = cols["icount"]
    # regs is a ClickHouse array literal "[v1,v2,...,v31]"; x10 (a0) is
    # element 10 (x1..x31, 1-indexed -- schema.sql's convention).
    regs = [int(x) for x in cols["regs"].strip("[]").split(",")]
    a0 = regs[9]  # x10

    if not halted:
        return False, f"did not halt within {max_instructions} instructions (pc={cols['pc']})"
    if halt_reason != "ECALL":
        return False, f"expected a clean ECALL exit, got {halt_reason} at pc={cols['pc']} (icount={icount})"
    if a0 != 0:
        failing_testnum = (a0 - 1) >> 1
        return False, f"test case {failing_testnum} failed (a0=0x{a0:x}, icount={icount})"
    return True, f"icount={icount}"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", default="clickdoom")
    ap.add_argument("--client", default="clickhouse-client")
    ap.add_argument("--fixtures-dir", default=str(FIXTURES_DIR))
    ap.add_argument("--max-instructions", type=int, default=DEFAULT_MAX_INSTRUCTIONS)
    args = ap.parse_args()

    client_cmd = args.client.split() + [
        "--host", args.host, "--port", str(args.port), "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        client_cmd += ["--password", args.password]

    def ch(sql):
        result = subprocess.run(client_cmd, input=sql, text=True, capture_output=True)
        if result.returncode != 0:
            raise RuntimeError(result.stderr)
        return result.stdout

    fixtures = sorted(Path(args.fixtures_dir).glob("*.bin"))
    if not fixtures:
        print(f"::error::no fixtures found in {args.fixtures_dir}", file=sys.stderr)
        return 1

    passed, failed = 0, []
    for fixture in fixtures:
        ok, detail = run_one_fixture(ch, fixture, args.max_instructions)
        status = "PASS" if ok else "FAIL"
        print(f"{status} {fixture.stem}: {detail}", file=sys.stderr)
        if ok:
            passed += 1
        else:
            failed.append(fixture.stem)

    total = len(fixtures)
    print(f"riscv-tests inside ClickHouse: {passed}/{total} passed")
    if failed:
        print(f"::error::failed: {', '.join(failed)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
