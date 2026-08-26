"""Unit tests for #23's fold (executor/fold.py) against the independent
Python reference model in executor/tests/reference.py.

Not riscv-tests, not a SPEC §7 differential run -- those need sqlcpu's
decode (#18/#19) and refemu (#11), neither landed yet. This checks a
narrower, real thing: does the fold correctly implement the collapsed
op_id/halt semantics #23's design claims, for hand-built instruction
streams covering every arm and every halt reason.

Requires `just up` (clickdoom-ch reachable via `docker exec`).

Run: cd executor && uv run pytest tests/test_fold.py -v
"""
import json
import subprocess
import sys

import pytest

sys.path.insert(0, ".")
import fold  # noqa: E402
from tests import reference  # noqa: E402

CONTAINER = "clickdoom-ch"
DB = "clickdoom_executor"
RAM_BASE = 0x8000_0000
RAM_BASE_WORD = RAM_BASE >> 2


def ch(sql, fmt=None):
    cmd = ["docker", "exec", "-i", CONTAINER, "clickhouse-client", "--multiquery"]
    if fmt:
        cmd += ["--format", fmt]
    r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"clickhouse-client failed:\n{sql}\n{r.stderr}")
    return r.stdout


@pytest.fixture(scope="session", autouse=True)
def schema():
    with open("schema_fixture.sql") as f:
        ch(f.read())


def next_pow2(n):
    p = 1
    while p < n:
        p *= 2
    return p


def run_case(insns, ram=None, pc0=0, regs0=None, k=None, hwm=10_000, ram_words=None):
    """insns: list[reference.Insn]. ram: dict relative-word-index -> value.
    Returns (actual, expected) accumulator dicts."""
    decn = next_pow2(max(len(insns), 8))
    padded = list(insns) + [reference.Insn(op_id=reference.OP_ILLEGAL, raw=0xBAD00000 + len(insns) + i)
                             for i in range(decn - len(insns))]
    ram_words = ram_words or decn
    assert ram_words >= decn, "RAM must contain the text window in this fixture"

    ch(f"TRUNCATE TABLE {DB}.decoded; TRUNCATE TABLE {DB}.ram;")
    rows = []
    for i, ins in enumerate(padded):
        rows.append(f"({RAM_BASE_WORD + i},{ins.op_id},{ins.rd},{ins.rs1},{ins.rs2},"
                     f"{reference.u32(ins.imm)},{ins.target},{ins.width_mask},{ins.sign_bit},"
                     f"{reference.u32(ins.raw)})")
    ch(f"INSERT INTO {DB}.decoded (word_addr,op_id,rd,rs1,rs2,imm,target,width_mask,sign_bit,raw) "
       f"VALUES {','.join(rows)}")

    ram = ram or {}
    ram_rows = [f"({RAM_BASE_WORD + w},{v},0)" for w, v in ram.items()]
    if ram_rows:
        ch(f"INSERT INTO {DB}.ram (word_addr,value,version) VALUES {','.join(ram_rows)}")
    else:
        # ReplacingMergeTree FINAL over zero rows -> groupArray returns [],
        # and RAM[i] would fail; the fold indexes captured RAM with a
        # masked-safe index, but the array itself must have ram_words rows.
        ch(f"INSERT INTO {DB}.ram (word_addr,value,version) "
           f"SELECT {RAM_BASE_WORD} + number, 0, 0 FROM numbers({ram_words})")
    if ram_rows:
        # fill remaining words with 0 so the captured array has exactly ram_words entries
        present = set(ram.keys())
        missing = [f"({RAM_BASE_WORD + w},0,0)" for w in range(ram_words) if w not in present]
        if missing:
            ch(f"INSERT INTO {DB}.ram (word_addr,value,version) VALUES {','.join(missing)}")

    k = k if k is not None else len(insns)
    sql = fold.select_only(k, 0, decn, decn, ram_words, hwm, pc0=pc0, regs0=regs0)
    out = json.loads(ch(sql, fmt="JSONEachRow").strip().splitlines()[0])
    insns = padded  # so the reference model sees the exact same padding, including
                     # the ILLEGAL entries a stray jump target can land on
    actual = dict(
        pcidx=int(out["pcidx"]), regs=[int(x) for x in out["regs"]],
        wl_addr=[int(x) for x in out["wl_addr"]], wl_val=[int(x) for x in out["wl_val"]],
        wl_icount=[int(x) for x in out["wl_icount"]], stopped=int(out["stopped"]),
        halted=int(out["halted"]), halt_reason=int(out["halt_reason"]),
        halt_pc=int(out["halt_pc"]), halt_extra=int(out["halt_extra"]),
        retired=int(out["retired"]))

    expected = reference.run(insns, RAM_BASE, ram_words, 0, decn, regs0=regs0, pc0=pc0, k=k,
                              hwm=hwm, ram0=ram)
    return actual, expected


def I(**kw):
    return reference.Insn(**kw)


def test_alu_arms_straight_line():
    # x1=5, x2=3 via addi (op 0, rs1=x0), then exercise every ALU arm writing x3.
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=5),     # addi x1, x0, 5
        I(op_id=0, rd=2, rs1=0, rs2=0, imm=3),     # addi x2, x0, 3
        I(op_id=1, rd=3, rs1=1, rs2=2, imm=0),     # sub
        I(op_id=2, rd=3, rs1=1, rs2=2, imm=0),     # sll
        I(op_id=3, rd=3, rs1=1, rs2=2, imm=0),     # slt
        I(op_id=4, rd=3, rs1=1, rs2=2, imm=0),     # sltu
        I(op_id=5, rd=3, rs1=1, rs2=2, imm=0),     # xor
        I(op_id=6, rd=3, rs1=1, rs2=2, imm=0),     # srl
        I(op_id=7, rd=3, rs1=1, rs2=2, imm=0),     # sra
        I(op_id=8, rd=3, rs1=1, rs2=2, imm=0),     # or
        I(op_id=9, rd=3, rs1=1, rs2=2, imm=0),     # and
        I(op_id=10, rd=3, rs1=1, rs2=2, imm=0),    # mul
        I(op_id=11, rd=3, rs1=1, rs2=2, imm=0),    # mulh
        I(op_id=12, rd=3, rs1=1, rs2=2, imm=0),    # mulhsu
        I(op_id=13, rd=3, rs1=1, rs2=2, imm=0),    # mulhu
        I(op_id=14, rd=3, rs1=1, rs2=2, imm=0),    # div
        I(op_id=15, rd=3, rs1=1, rs2=2, imm=0),    # divu
        I(op_id=16, rd=3, rs1=1, rs2=2, imm=0),    # rem
        I(op_id=17, rd=3, rs1=1, rs2=2, imm=0),    # remu
    ]
    actual, expected = run_case(insns)
    assert actual == expected


def test_div_by_zero_and_x0_discard():
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=7),
        I(op_id=14, rd=0, rs1=1, rs2=0, imm=0),    # div by (x0=0) result into x0 -- must discard
        I(op_id=15, rd=2, rs1=1, rs2=0, imm=0),    # divu by zero -> 0xFFFFFFFF
    ]
    actual, expected = run_case(insns)
    assert actual == expected
    assert actual["regs"][0] == 0
    assert actual["regs"][2] == 0xFFFFFFFF


def test_store_then_load_shadows_ram():
    # Addresses are absolute (SPEC §2: RAM lives at RAM_BASE); rs1=x0=0
    # contributes nothing, so imm must itself be RAM_BASE + offset -- x0
    # does not magically hold RAM_BASE. decn pads to 8 (next_pow2(max(3,8))),
    # so word indices 0-7 are "text" -- store at word 8 so this exercises the
    # write-log, not SELF_MODIFY.
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=100),                       # x1 = 100
        I(op_id=19, rd=0, rs1=0, rs2=1, imm=RAM_BASE + 32,             # sw x1, (RAM_BASE+32)(x0)
          width_mask=0xFFFFFFFF, sign_bit=0),
        I(op_id=18, rd=2, rs1=0, rs2=0, imm=RAM_BASE + 32,             # lw x2, (RAM_BASE+32)(x0)
          width_mask=0xFFFFFFFF, sign_bit=0),
    ]
    actual, expected = run_case(insns, ram={8: 999}, ram_words=16)  # word 8 = byte offset 32
    assert actual == expected
    assert actual["regs"][2] == 100  # write-log shadowed the stale RAM value
    assert actual["wl_icount"] == [2]  # per-store icount, not batch icount


def test_load_byte_sign_extend():
    insns = [
        I(op_id=18, rd=1, rs1=0, rs2=0, imm=RAM_BASE, width_mask=0xFF, sign_bit=0x80),  # lb
    ]
    actual, expected = run_case(insns, ram={0: 0xFFFFFF80})  # low byte 0x80
    assert actual == expected
    assert actual["regs"][1] == 0xFFFFFF80  # sign-extended -128


def test_branches_and_jumps():
    for op_id, a, b, taken in [(20, 5, 5, True), (20, 5, 6, False),
                                (21, 5, 6, True), (21, 5, 5, False),
                                (22, -1, 1, True), (23, -1, 1, False),
                                (24, 1, 0xFFFFFFFF, True), (25, 1, 0xFFFFFFFF, False)]:
        insns = [
            I(op_id=0, rd=1, rs1=0, rs2=0, imm=a & 0xFFFFFFFF),
            I(op_id=0, rd=2, rs1=0, rs2=0, imm=b & 0xFFFFFFFF),
            I(op_id=op_id, rs1=1, rs2=2, target=7),
            I(op_id=0, rd=5, rs1=0, rs2=0, imm=111),   # fallthrough marker
        ]
        actual, expected = run_case(insns, k=4)
        assert actual == expected, (op_id, a, b, taken)
        assert (actual["pcidx"] == 7) == taken


def test_jal_jalr():
    insns = [
        I(op_id=26, rd=1, target=99, imm=99),   # jal x1, 99  (link value precomputed = 99)
        I(op_id=0, rd=9, rs1=0, rs2=0, imm=111),  # skipped
    ]
    actual, expected = run_case(insns, k=1)
    assert actual == expected
    assert actual["pcidx"] == 99  # jal/jalr targets are pre-decoded absolute word
    assert actual["regs"][1] == 99  # indices (ADR-0002) -- not masked, unlike fallthrough


def test_halt_ecall_ebreak_csr_illegal():
    for op_id, reason in [(reference.OP_ECALL, reference.HALT_ECALL),
                           (reference.OP_EBREAK, reference.HALT_EBREAK),
                           (reference.OP_CSR, reference.HALT_CSR)]:
        insns = [I(op_id=0, rd=1, rs1=0, rs2=0, imm=1), I(op_id=op_id)]
        actual, expected = run_case(insns, k=2)
        assert actual == expected
        assert actual["halted"] == 1 and actual["halt_reason"] == reason
        assert actual["halt_pc"] == 1          # frozen at the faulting insn
        assert actual["pcidx"] == 1            # did not advance past it
        assert actual["regs"][1] == 1          # prior instruction still retired


def test_halt_illegal_carries_raw_word():
    insns = [I(op_id=0, rd=1, rs1=0, rs2=0, imm=1)]  # padding fills with OP_ILLEGAL, raw=0xBAD00001
    actual, expected = run_case(insns, k=2)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_ILLEGAL_INSN
    assert actual["halt_extra"] == 0xBAD00001


def test_halt_bad_addr():
    insns = [
        I(op_id=18, rd=1, rs1=0, rs2=0, imm=RAM_BASE - 4, width_mask=0xFFFFFFFF, sign_bit=0),  # just before RAM
    ]
    actual, expected = run_case(insns, ram_words=8, k=1)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_BAD_ADDR
    assert actual["halt_extra"] == (RAM_BASE - 4) & 0xFFFFFFFF
    assert actual["regs"][1] == 0  # load did not retire


def test_halt_misaligned():
    insns = [
        I(op_id=18, rd=1, rs1=0, rs2=0, imm=RAM_BASE + 2, width_mask=0xFFFFFFFF, sign_bit=0),  # +2: misaligned word
    ]
    actual, expected = run_case(insns, k=1)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_MISALIGNED


def test_halt_self_modify():
    insns = [
        I(op_id=19, rd=0, rs1=0, rs2=0, imm=RAM_BASE, width_mask=0xFFFFFFFF, sign_bit=0),  # sw x0, RAM_BASE(x0): word 0 is text
    ]
    actual, expected = run_case(insns, k=1)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_SELF_MODIFY
    assert len(actual["wl_addr"]) == 0  # the faulting store never retired


def test_high_water_mark_stops_without_halting():
    # decn will pad up to 8 (next_pow2(max(6,8))), so word indices 0-7 are
    # "text" -- store at word 8+ so this exercises the high-water mark, not
    # SELF_MODIFY.
    insns = [
        I(op_id=19, rd=0, rs1=0, rs2=0, imm=RAM_BASE + (8 + i) * 4, width_mask=0xFFFFFFFF, sign_bit=0)
        for i in range(6)
    ]
    actual, expected = run_case(insns, ram_words=16, hwm=3, k=6)
    assert actual == expected
    assert actual["stopped"] == 1 and actual["halted"] == 0
    assert len(actual["wl_addr"]) == 3
    assert actual["retired"] == 3
    assert actual["wl_icount"] == [1, 2, 3]


def test_stopped_step_is_a_no_op():
    insns = [I(op_id=28), I(op_id=0, rd=1, rs1=0, rs2=0, imm=42)]  # ecall, then something after
    actual, expected = run_case(insns, k=2)
    assert actual == expected
    assert actual["regs"][1] == 0  # never reached
    assert actual["retired"] == 0
