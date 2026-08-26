"""Boot report tests (issue #16). Synthetic tiny programs stand in for a
real ROM image (which doesn't exist yet) to exercise all three outcomes
and the instruction-mix histogram; `classify()` is checked against every
encoder in `tests/asm.py` so the histogram vocabulary can't silently drift
from what the interpreter actually executes.
"""

import pytest

from refemu.boot import BootReport, boot, classify, format_report
from refemu.memory import MMIO_BASE, RAM_BASE
from refemu.mmio import FRAME_COMMIT

from . import asm


def _image(words: list[int]) -> bytes:
    return b"".join(w.to_bytes(4, "little") for w in words)


@pytest.mark.parametrize(
    "encoder,args,expected",
    [
        (asm.lui, (1, 0x1000), "lui"),
        (asm.auipc, (1, 0x1), "auipc"),
        (asm.jal, (1, 0x100), "jal"),
        (asm.jalr, (1, 2, 0), "jalr"),
        (asm.beq, (1, 2, 0x10), "beq"),
        (asm.bne, (1, 2, 0x10), "bne"),
        (asm.blt, (1, 2, 0x10), "blt"),
        (asm.bge, (1, 2, 0x10), "bge"),
        (asm.bltu, (1, 2, 0x10), "bltu"),
        (asm.bgeu, (1, 2, 0x10), "bgeu"),
        (asm.lb, (1, 2, 0), "lb"),
        (asm.lh, (1, 2, 0), "lh"),
        (asm.lw, (1, 2, 0), "lw"),
        (asm.lbu, (1, 2, 0), "lbu"),
        (asm.lhu, (1, 2, 0), "lhu"),
        (asm.sb, (1, 2, 0), "sb"),
        (asm.sh, (1, 2, 0), "sh"),
        (asm.sw, (1, 2, 0), "sw"),
        (asm.addi, (1, 2, 3), "addi"),
        (asm.slti, (1, 2, 3), "slti"),
        (asm.sltiu, (1, 2, 3), "sltiu"),
        (asm.xori, (1, 2, 3), "xori"),
        (asm.ori, (1, 2, 3), "ori"),
        (asm.andi, (1, 2, 3), "andi"),
        (asm.slli, (1, 2, 3), "slli"),
        (asm.srli, (1, 2, 3), "srli"),
        (asm.srai, (1, 2, 3), "srai"),
        (asm.add, (1, 2, 3), "add"),
        (asm.sub, (1, 2, 3), "sub"),
        (asm.sll, (1, 2, 3), "sll"),
        (asm.slt, (1, 2, 3), "slt"),
        (asm.sltu, (1, 2, 3), "sltu"),
        (asm.xor_, (1, 2, 3), "xor"),
        (asm.srl, (1, 2, 3), "srl"),
        (asm.sra, (1, 2, 3), "sra"),
        (asm.or_, (1, 2, 3), "or"),
        (asm.and_, (1, 2, 3), "and"),
        (asm.mul, (1, 2, 3), "mul"),
        (asm.mulh, (1, 2, 3), "mulh"),
        (asm.mulhsu, (1, 2, 3), "mulhsu"),
        (asm.mulhu, (1, 2, 3), "mulhu"),
        (asm.div, (1, 2, 3), "div"),
        (asm.divu, (1, 2, 3), "divu"),
        (asm.rem, (1, 2, 3), "rem"),
        (asm.remu, (1, 2, 3), "remu"),
        (asm.fence, (), "fence"),
        (asm.ecall, (), "ecall"),
        (asm.ebreak, (), "ebreak"),
        (asm.csrrw, (1, 2, 0x300), "csr"),
    ],
)
def test_classify_matches_every_encoder(encoder, args, expected):
    assert classify(encoder(*args)) == expected


def test_classify_reserved_opcode_is_illegal():
    assert classify(asm.RESERVED_OPCODE) == "illegal"


def test_boot_reaches_frame_commit():
    words = [
        asm.lui(1, MMIO_BASE >> 12),  # x1 = MMIO_BASE
        asm.addi(2, 0, 7),  # x2 = 7 (frame number)
        asm.sw(1, 2, FRAME_COMMIT),  # MMIO[FRAME_COMMIT] = 7
    ]
    report = boot(_image(words), max_instructions=1000)
    assert report.outcome == "frame_commit"
    assert report.frame_no == 7
    assert report.icount == 3
    assert report.retired_pcs == [RAM_BASE, RAM_BASE + 4, RAM_BASE + 8]
    assert report.histogram == {"lui": 1, "addi": 1, "sw": 1}


def test_boot_reports_fault():
    words = [asm.addi(1, 0, 1), asm.RESERVED_OPCODE]
    report = boot(_image(words), max_instructions=1000)
    assert report.outcome == "halt"
    assert report.halt_reason == "ILLEGAL_INSN"
    assert report.halt_pc == RAM_BASE + 4
    assert report.halt_insn == asm.RESERVED_OPCODE
    assert report.icount == 1  # only the addi retired
    assert report.retired_pcs == [RAM_BASE]
    assert report.histogram == {"addi": 1}


def test_boot_reports_budget_exhausted():
    words = [asm.jal(0, 0)]  # infinite self-loop, never halts or commits
    report = boot(_image(words), max_instructions=50)
    assert report.outcome == "budget_exhausted"
    assert report.icount == 50
    assert report.histogram == {"jal": 50}
    assert len(report.retired_pcs) == 20  # capped at RETIRED_PC_HISTORY
    assert all(pc == RAM_BASE for pc in report.retired_pcs)


def test_format_report_includes_key_fields():
    report = BootReport(
        outcome="halt",
        icount=42,
        histogram={"addi": 40, "ecall": 2},
        retired_pcs=[RAM_BASE, RAM_BASE + 4],
        halt_reason="ECALL",
        halt_pc=RAM_BASE + 8,
        halt_insn=0x00000073,
    )
    text = format_report(report)
    assert "FAULT: ECALL" in text
    assert f"0x{RAM_BASE + 8:08x}" in text
    assert "0x00000073" in text
    assert "addi" in text and "ecall" in text
    assert "42" in text  # icount appears somewhere sensible


def test_format_report_frame_commit_outcome():
    report = BootReport(outcome="frame_commit", icount=100, frame_no=1)
    text = format_report(report)
    assert "CLEAN RUN" in text
    assert "frame 1" in text


def test_format_report_budget_exhausted_outcome():
    report = BootReport(outcome="budget_exhausted", icount=100)
    text = format_report(report)
    assert "BUDGET EXHAUSTED" in text
