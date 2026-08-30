"""SPEC §3 MMIO width-gating tests (#152): a non-word-width access at one
of the five MMIO register addresses must read 0 (load) / be silently
ignored (store), exactly like a non-register offset in the same window --
never the register's real semantics (TICKS_MS's clock, KEYQ's pop, EXIT's
halt, PUTCHAR's console push, FRAME_COMMIT's frame commit).

Why this is its own file, not more cases in test_fold.py: every case
there is checked against executor/tests/reference.py's independent Python
model via run_case()'s `actual == expected` comparison -- but reference.py
has no MMIO model at all (grep it; HALT_EXIT isn't even in its halt-reason
enum). That is not incidental to #152 -- it is *why* #152 shipped
unnoticed: "zero MMIO test cases... no dedicated MMIO test file" was the
issue's own diagnosis of how a plain, ordinary, address-only dispatch bug
sat undetected through review and every fold.py test run. So this file
checks fold.py's raw select_only() output directly against SPEC §3's text
(and refemu/src/refemu/mmio.py's own width gate, #87/#90 -- the two
engines are required to agree here, not just this file's opinion of what
SPEC says), rather than against a second model that would need building
from scratch to cover five registers it has never touched.

Every case below is paired: a narrow access (must NOT hit register
semantics) and a same-address word access (must still work) -- the word
case is the regression guard that proves a fix didn't overcorrect into
breaking the real path DOOM actually uses.

Requires a reachable ClickHouse: see test_fold.py's matching comment for
the CLICKHOUSE_HOST / docker-exec-vs-network split (CI vs local dev).

Run: cd executor && uv run pytest tests/test_mmio.py -v
"""
import json
import os
import subprocess
import sys

import pytest

sys.path.insert(0, ".")
import config  # noqa: E402
import fold  # noqa: E402
from tests import reference  # noqa: E402

CONTAINER = "clickdoom-ch"
DB = "clickdoom_executor"
RAM_BASE = config.RAM_BASE
RAM_BASE_WORD = RAM_BASE >> 2

# See test_fold.py's matching CH_HOST comment -- same switch, same reasoning.
CH_HOST = os.environ.get("CLICKHOUSE_HOST")


def ch(sql, fmt=None):
    if CH_HOST:
        cmd = ["clickhouse-client",
               "--host", CH_HOST,
               "--port", os.environ.get("CLICKHOUSE_PORT", "9000"),
               "--user", os.environ.get("CLICKHOUSE_USER", "default"),
               "--database", os.environ.get("CLICKHOUSE_DATABASE", "clickdoom"),
               "--multiquery"]
        if os.environ.get("CLICKHOUSE_PASSWORD"):
            cmd += ["--password", os.environ["CLICKHOUSE_PASSWORD"]]
    else:
        cmd = ["docker", "exec", "-i", CONTAINER, "clickhouse-client", "--multiquery"]
    if fmt:
        cmd += ["--format", fmt]
    r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"clickhouse-client failed:\n{sql}\n{r.stderr}")
    return r.stdout


def next_pow2(n):
    p = 1
    while p < n:
        p *= 2
    return p


@pytest.fixture(scope="session", autouse=True)
def schema():
    # Same fixture as test_fold.py -- duplicated, not imported, matching
    # this test package's existing convention (test_commit.py duplicates
    # its own ch()/setup rather than sharing via conftest.py). Idempotent
    # (schema_fixture.sql is DROP+CREATE), so running it twice across two
    # test files in one session is harmless.
    with open("schema_fixture.sql") as f:
        ch(f.read())


def I(**kw):  # noqa: E743 - one letter for one instruction, so a table of them reads as a table
    return reference.Insn(**kw)


def run_mmio_case(insns, k=None, icount0=0, keyq_events=None, pc0=None, regs0=None, hwm=10_000):
    """Like test_fold.run_case(), minus the reference.py comparison (see
    module docstring for why): returns fold.py's raw select_only() output
    for direct assertion against SPEC §3, not `actual == expected`.

    keyq_events: list of key_event values, loaded into input_queue in
    event_seq order -- KEYQT (fold.py's decode_with()) reads exactly this
    shape.
    """
    decn = next_pow2(max(len(insns), 8))
    padded = list(insns) + [reference.Insn(op_id=reference.OP_ILLEGAL, raw=0xBAD00000 + len(insns) + i)
                             for i in range(decn - len(insns))]
    ram_words = decn

    ch(f"TRUNCATE TABLE {DB}.decoded; TRUNCATE TABLE {DB}.ram; TRUNCATE TABLE {DB}.input_queue;")
    rows = []
    for i, ins in enumerate(padded):
        rows.append(f"({RAM_BASE_WORD + i},{ins.op_id},{ins.rd},{ins.rs1},{ins.rs2},"
                     f"{reference.u32(ins.imm)},{reference.u32(ins.target)},{ins.width_mask},{ins.sign_bit},"
                     f"{reference.u32(ins.raw)})")
    ch(f"INSERT INTO {DB}.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) "
       f"VALUES {','.join(rows)}")
    # Dense zero-filled RAM over the text window -- MMIO addresses are
    # 0x1000_0000, far outside RAM_BASE (0x8000_0000), so none of these
    # instructions ever touch it; only decode/RAM density (#81) needs
    # satisfying here, same as test_fold.py's run_case().
    ch(f"INSERT INTO {DB}.ram (word_addr,value,version) "
       f"SELECT {RAM_BASE_WORD} + number, 0, 0 FROM numbers({ram_words})")

    if keyq_events:
        kq_rows = ",".join(f"({seq},{ev},0)" for seq, ev in enumerate(keyq_events))
        ch(f"INSERT INTO {DB}.input_queue (event_seq,key_event,consumed) VALUES {kq_rows}")

    k = k if k is not None else len(insns)
    sql = fold.select_only(k, 0, decn, decn, ram_words, hwm, pc0=pc0, regs0=regs0, icount0=icount0)
    out = json.loads(ch(sql, fmt="JSONEachRow").strip().splitlines()[0])
    return dict(
        pc=int(out["pc"]), regs=[int(x) for x in out["regs"]],
        stopped=int(out["stopped"]), halted=int(out["halted"]),
        halt_reason=int(out["halt_reason"]), halt_pc=int(out["halt_pc"]),
        halt_extra=int(out["halt_extra"]), retired=int(out["retired"]),
        console_bytes=[int(x) for x in out["console_bytes"]],
        keyq_pos=int(out["keyq_pos"]), frame_no=int(out["frame_no"]),
        frame_committed=int(out["frame_committed"]),
    )


# icount0 chosen so TICKS_MS = intDiv(icount0, IPMS_DEFAULT) is nonzero
# (IPMS_DEFAULT=10_000) -- a test that only ever observed TICKS_MS=0 could
# pass by accident (byte load happens to equal the never-checked-for-real
# word value) rather than because the width gate actually held.
TICKS_ICOUNT0 = 53_000
EXPECTED_TICKS_MS = TICKS_ICOUNT0 // config.IPMS_DEFAULT
assert EXPECTED_TICKS_MS not in (0,), "test needs a nonzero clock value to be meaningful"


def test_byte_load_at_ticks_ms_reads_zero_not_the_clock():
    insns = [I(op_id=config.OP_LOAD, rd=1, rs1=0, rs2=0,
                imm=config.MMIO_BASE + config.MMIO_TICKS_MS,
                width_mask=0xFF, sign_bit=0)]  # lb
    actual = run_mmio_case(insns, icount0=TICKS_ICOUNT0)
    assert actual["regs"][0] == 0  # x1 -- NOT EXPECTED_TICKS_MS
    assert actual["halted"] == 0 and actual["retired"] == 1


def test_word_load_at_ticks_ms_still_reads_the_clock():
    # Positive control for the case above -- proves the fix didn't also
    # break the real, word-width path DOOM's own TICKS_MS read uses.
    insns = [I(op_id=config.OP_LOAD, rd=1, rs1=0, rs2=0,
                imm=config.MMIO_BASE + config.MMIO_TICKS_MS,
                width_mask=0xFFFFFFFF, sign_bit=0)]  # lw
    actual = run_mmio_case(insns, icount0=TICKS_ICOUNT0)
    assert actual["regs"][0] == EXPECTED_TICKS_MS


# All the store-side cases below seed x1 with a leading `addi x1, x0, N`
# rather than passing regs0 to run_mmio_case()/select_only(): regs0's SQL
# literal array (`[N,0,0,...]`) gets its type inferred from the literal
# values, and every value small enough to matter here (an exit code, a
# console byte, a frame number) infers as Array(UInt8), not the
# accumulator's real Array(UInt32) -- arrayFold then rejects the whole
# query (`Return type of lambda function must be the same as the
# accumulator type`). Hit running this file's first draft against it --
# an independent rediscovery of #156 (filed earlier from a different
# direction, rom/bench/canonical_throughput's own regs0 call), not a new
# bug; noted on that issue rather than refiled. Unrelated to #152 itself.
# These tests route around it the same way every existing test_fold.py
# case already does: seed via addi, never regs0 with small values.
def test_byte_store_to_exit_does_not_halt():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=42),                        # addi x1, x0, 42
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_EXIT,
          width_mask=0xFF, sign_bit=0),                                 # sb x1, EXIT(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["halted"] == 0
    assert actual["retired"] == 2
    assert actual["pc"] == RAM_BASE + 8  # advanced past both, not frozen at a fault


def test_word_store_to_exit_still_halts():
    # Positive control: SPEC §3's real EXIT path must survive the fix.
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=42),                        # addi x1, x0, 42
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_EXIT,
          width_mask=0xFFFFFFFF, sign_bit=0),                           # sw x1, EXIT(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["halted"] == 1
    assert actual["halt_reason"] == config.HALT_EXIT
    assert actual["halt_extra"] == 42  # exit_code, per SPEC §3 -- the stored value
    assert actual["halt_pc"] == RAM_BASE + 4  # frozen at the store, not the addi
    assert actual["pc"] == RAM_BASE + 4       # did not advance past it


def test_byte_store_to_putchar_does_not_push_console_byte():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=ord("Q")),                  # addi x1, x0, 'Q'
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_PUTCHAR,
          width_mask=0xFF, sign_bit=0),                                 # sb x1, PUTCHAR(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["console_bytes"] == []
    assert actual["halted"] == 0 and actual["retired"] == 2


def test_word_store_to_putchar_still_pushes_console_byte():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=ord("Q")),                  # addi x1, x0, 'Q'
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_PUTCHAR,
          width_mask=0xFFFFFFFF, sign_bit=0),                           # sw x1, PUTCHAR(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["console_bytes"] == [ord("Q")]


def test_byte_store_to_frame_commit_does_not_commit_a_frame():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=7),                         # addi x1, x0, 7
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_FRAME_COMMIT,
          width_mask=0xFF, sign_bit=0),                                 # sb x1, FRAME_COMMIT(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["frame_committed"] == 0
    assert actual["halted"] == 0 and actual["retired"] == 2


def test_word_store_to_frame_commit_still_commits_a_frame():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=7),                         # addi x1, x0, 7
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_FRAME_COMMIT,
          width_mask=0xFFFFFFFF, sign_bit=0),                           # sw x1, FRAME_COMMIT(x0)
    ]
    actual = run_mmio_case(insns, k=2)
    assert actual["frame_committed"] == 1
    assert actual["frame_no"] == 7  # RS2V, per SPEC §3: the stored value is the frame number


def test_frame_commit_stops_the_batch_without_halting():
    # #223: SPEC §6 ("A batch ends early on: halt, FRAME_COMMIT write, or
    # write-log high-water mark") -- fold.py set frame_committed but never
    # folded it into the fold's own `stopped` termination condition, so a
    # frame-committing batch ran a full K past the commit instead of
    # stopping there. k=3 here, one more than the commit point (index 1),
    # mirroring test_fold.py's test_high_water_mark_stops_without_halting
    # shape -- the third instruction (which would set x2) must NOT execute
    # if the batch actually stopped at the commit rather than merely
    # recording it.
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=7),                         # addi x1, x0, 7
        I(op_id=config.OP_STORE, rd=0, rs1=0, rs2=1,
          imm=config.MMIO_BASE + config.MMIO_FRAME_COMMIT,
          width_mask=0xFFFFFFFF, sign_bit=0),                           # sw x1, FRAME_COMMIT(x0)
        I(op_id=0, rd=2, rs1=0, rs2=0, imm=99),                        # addi x2, x0, 99 (must not retire)
    ]
    actual = run_mmio_case(insns, k=3)
    assert actual["frame_committed"] == 1
    assert actual["frame_no"] == 7
    assert actual["stopped"] == 1 and actual["halted"] == 0  # stopped, not faulted
    assert actual["retired"] == 2  # the addi and the commit store -- not the third insn
    assert actual["regs"][1] == 0  # x2 -- never reached


def test_byte_load_at_keyq_does_not_pop_the_queue():
    insns = [
        I(op_id=config.OP_LOAD, rd=1, rs1=0, rs2=0,
          imm=config.MMIO_BASE + config.MMIO_KEYQ,
          width_mask=0xFF, sign_bit=0),  # lb x1, KEYQ(x0)
    ]
    actual = run_mmio_case(insns, keyq_events=[0x1234])
    assert actual["regs"][0] == 0  # NOT the queued event's low byte
    assert actual["keyq_pos"] == 0  # not advanced -- the event is still there


def test_word_load_at_keyq_still_pops_the_queue():
    insns = [
        I(op_id=config.OP_LOAD, rd=1, rs1=0, rs2=0,
          imm=config.MMIO_BASE + config.MMIO_KEYQ,
          width_mask=0xFFFFFFFF, sign_bit=0),  # lw x1, KEYQ(x0)
    ]
    actual = run_mmio_case(insns, keyq_events=[0x1234])
    assert actual["regs"][0] == 0x1234
    assert actual["keyq_pos"] == 1
