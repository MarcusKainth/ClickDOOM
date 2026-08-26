"""Unit tests for #23's fold (executor/fold.py) against the independent
Python reference model in executor/tests/reference.py.

Not riscv-tests, not a SPEC §7 differential run -- those need sqlcpu's
decode/execute (#18/#19, PRs #42/#46/#49, not yet merged) and refemu's
checkpoint emitter (#15), not this fold's own tests. This checks a
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


def run_case(insns, ram=None, pc0=None, regs0=None, k=None, hwm=10_000, ram_words=None):
    """insns: list[reference.Insn]. ram: dict relative-word-index -> value.
    pc0 defaults to RAM_BASE (a byte address, matching fold.py/reference.py's
    own default and SPEC §1's reset value) -- not 0, not a word index.
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
                     f"{reference.u32(ins.imm)},{reference.u32(ins.target)},{ins.width_mask},{ins.sign_bit},"
                     f"{reference.u32(ins.raw)})")
    # Column names match sqlcpu/schema.sql (id/tgt/mk/sg), not SPEC §5's prose.
    ch(f"INSERT INTO {DB}.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) "
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
        pc=int(out["pc"]), regs=[int(x) for x in out["regs"]],
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
    # regs[0] = x1 (31-element, 1-indexed x1..x31 -- sqlcpu's schema.sql,
    # PR #42; no slot for x0 at all). x1 must be unaffected by the
    # discarded x0 write; x2 (regs[1]) is the divu-by-zero result.
    assert actual["regs"][0] == 7
    assert actual["regs"][1] == 0xFFFFFFFF


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
    assert actual["regs"][1] == 100  # x2 (regs[1]) -- write-log shadowed the stale RAM value
    assert actual["wl_icount"] == [2]  # per-store icount, not batch icount


def test_load_byte_sign_extend():
    insns = [
        I(op_id=18, rd=1, rs1=0, rs2=0, imm=RAM_BASE, width_mask=0xFF, sign_bit=1),  # lb (sg is boolean)
    ]
    actual, expected = run_case(insns, ram={0: 0xFFFFFF80})  # low byte 0x80
    assert actual == expected
    assert actual["regs"][0] == 0xFFFFFF80  # x1 (regs[0]) -- sign-extended -128


def test_branches_and_jumps():
    # target is a byte address (word 7 -> RAM_BASE + 28), matching sqlcpu's
    # schema.sql/decode.sql and the PC-representation fix in this PR.
    target = RAM_BASE + 7 * 4
    for op_id, a, b, taken in [(20, 5, 5, True), (20, 5, 6, False),
                                (21, 5, 6, True), (21, 5, 5, False),
                                (22, -1, 1, True), (23, -1, 1, False),
                                (24, 1, 0xFFFFFFFF, True), (25, 1, 0xFFFFFFFF, False)]:
        insns = [
            I(op_id=0, rd=1, rs1=0, rs2=0, imm=a & 0xFFFFFFFF),
            I(op_id=0, rd=2, rs1=0, rs2=0, imm=b & 0xFFFFFFFF),
            I(op_id=op_id, rs1=1, rs2=2, target=target),
            I(op_id=0, rd=5, rs1=0, rs2=0, imm=111),   # fallthrough marker
        ]
        actual, expected = run_case(insns, k=4)
        assert actual == expected, (op_id, a, b, taken)
        assert (actual["pc"] == target) == taken


def test_jal_jalr():
    # jump target and link value (pc+4 as a byte address, computed live) are
    # independent -- conflating them into one decoded column was the bug
    # this PR fixed (see LINK_VALUE in fold.py). target is deliberately not
    # RAM_BASE+4 (what the link value would be), so a test that accidentally
    # still reads `target` for the link value would fail loudly rather than
    # coincidentally pass.
    target = RAM_BASE + 99 * 4
    insns = [
        I(op_id=26, rd=1, target=target, imm=0),   # jal x1, target
        I(op_id=0, rd=9, rs1=0, rs2=0, imm=111),  # skipped
    ]
    actual, expected = run_case(insns, k=1)
    assert actual == expected
    assert actual["pc"] == target  # jump target, from `tgt` (ADR-0002/PR #46) -- unclamped byte address
    assert actual["regs"][0] == RAM_BASE + 4  # x1 (regs[0]) = link value: pc0(RAM_BASE) + 4


def test_halt_ecall_ebreak_csr_illegal():
    for op_id, reason in [(reference.OP_ECALL, reference.HALT_ECALL),
                           (reference.OP_EBREAK, reference.HALT_EBREAK),
                           (reference.OP_CSR, reference.HALT_CSR)]:
        insns = [I(op_id=0, rd=1, rs1=0, rs2=0, imm=1), I(op_id=op_id)]
        actual, expected = run_case(insns, k=2)
        assert actual == expected
        assert actual["halted"] == 1 and actual["halt_reason"] == reason
        assert actual["halt_pc"] == RAM_BASE + 4   # frozen at the faulting insn (word 1)
        assert actual["pc"] == RAM_BASE + 4        # did not advance past it
        assert actual["regs"][0] == 1              # x1 (regs[0]) -- prior instruction still retired


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
    assert actual["regs"][0] == 0  # x1 (regs[0]) -- load did not retire


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
    assert actual["regs"][0] == 0  # x1 (regs[0]) -- never reached
    assert actual["retired"] == 0


def test_halt_jal_misaligned_target():
    # SPEC §1 / issue #37 (ruled, all three engines): a misaligned jump
    # target halts EAGERLY at the transferring instruction, not deferred to
    # whatever would fetch it next -- unreachable from a well-formed RV32IM
    # binary (jal/jalr/branch targets are always encoding-forced to at least
    # 2-byte alignment via bit 0, and a real toolchain never emits a target
    # with bit 1 set), which is exactly why this needs a test: an
    # unreachable path is never exercised by anything else, so this is the
    # only thing keeping the agreement real instead of assumed.
    target = RAM_BASE + 4 + 2  # word-aligned base + 2: bit 1 set, bit 0 clear
    insns = [
        I(op_id=26, rd=1, target=target, imm=0),   # jal x1, target (misaligned)
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=111),   # must not be reached
    ]
    actual, expected = run_case(insns, k=2)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_MISALIGNED
    assert actual["halt_extra"] == target
    assert actual["halt_pc"] == RAM_BASE     # the jal instruction's own pc, not the target
    assert actual["pc"] == RAM_BASE          # frozen there -- did not "complete" onto the target
    assert actual["regs"][0] == 0            # link value was NOT written -- rd did not update


def test_halt_jalr_misaligned_target():
    target_base = RAM_BASE + 4 + 2  # bit 1 set
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=target_base),  # x1 = target_base
        I(op_id=27, rd=2, rs1=1, rs2=0, imm=0),            # jalr x2, x1, 0 (misaligned)
    ]
    actual, expected = run_case(insns, k=2)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_MISALIGNED
    assert actual["halt_extra"] == target_base  # jalr clears bit 0 only, bit 1 survives
    assert actual["halt_pc"] == RAM_BASE + 4    # the jalr instruction's own pc
    assert actual["regs"][1] == 0               # x2 (regs[1]) -- link value not written


def test_branch_misaligned_target_only_halts_if_taken():
    target = RAM_BASE + 4 + 2  # bit 1 set
    # beq, not taken (5 != 6): the misaligned target is never used, so this
    # must NOT halt -- proves the check is gated on `would_jump`, not blanket.
    insns = [
        I(op_id=0, rd=1, rs1=0, rs2=0, imm=5),
        I(op_id=0, rd=2, rs1=0, rs2=0, imm=6),
        I(op_id=20, rs1=1, rs2=2, target=target),  # beq x1, x2, target -- not taken
        I(op_id=0, rd=3, rs1=0, rs2=0, imm=222),
    ]
    actual, expected = run_case(insns, k=4)
    assert actual == expected
    assert actual["halted"] == 0
    assert actual["regs"][2] == 222  # x3 (regs[2]) -- fallthrough executed normally

    # Same instructions, but beq now taken (5 == 5): must halt MISALIGNED.
    insns[0] = I(op_id=0, rd=1, rs1=0, rs2=0, imm=5)
    insns[1] = I(op_id=0, rd=2, rs1=0, rs2=0, imm=5)
    actual, expected = run_case(insns, k=4)
    assert actual == expected
    assert actual["halted"] == 1 and actual["halt_reason"] == reference.HALT_MISALIGNED
    assert actual["halt_extra"] == target
