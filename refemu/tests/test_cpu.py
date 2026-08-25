"""Functional tests for the RV32I core (issue #11). One or two cases per
instruction, plus the signed/unsigned and sign/zero-extension edge cases
that are easy to get backwards."""

from refemu.memory import RAM_BASE

from .asm import (
    add,
    addi,
    and_,
    andi,
    auipc,
    beq,
    bge,
    bgeu,
    blt,
    bltu,
    bne,
    ecall,
    jal,
    jalr,
    lb,
    lbu,
    lh,
    lhu,
    lui,
    lw,
    or_,
    ori,
    sb,
    sh,
    sll,
    slli,
    slt,
    slti,
    sltiu,
    sltu,
    sra,
    srai,
    srl,
    srli,
    sub,
    sw,
    xor_,
    xori,
)
from .conftest import load


def test_reset_state(cpu):
    cpu.reset()
    assert cpu.pc == RAM_BASE
    assert cpu.regs == [0] * 32
    assert cpu.icount == 0


def test_x0_writes_discarded(cpu):
    load(cpu, [addi(0, 0, 42)])
    cpu.step()
    assert cpu.read_reg(0) == 0


def test_x0_stays_zero_across_many_writes(cpu):
    for _ in range(5):
        load(cpu, [addi(0, 0, -1)])  # would set x0 to 0xFFFFFFFF if honored
        cpu.step()
    assert cpu.read_reg(0) == 0


def test_icount_and_pc_advance(cpu):
    load(cpu, [addi(1, 0, 1), addi(1, 1, 1)])
    cpu.step()
    assert cpu.icount == 1
    assert cpu.pc == RAM_BASE + 4
    cpu.step()
    assert cpu.icount == 2
    assert cpu.read_reg(1) == 2


def test_lui(cpu):
    load(cpu, [lui(1, 0xABCDE)])
    cpu.step()
    assert cpu.read_reg(1) == 0xABCDE000


def test_auipc(cpu):
    load(cpu, [auipc(1, 0x1)])
    cpu.step()
    assert cpu.read_reg(1) == RAM_BASE + 0x1000


def test_jal_links_and_jumps(cpu):
    load(cpu, [jal(1, 0x100)])
    cpu.step()
    assert cpu.read_reg(1) == RAM_BASE + 4
    assert cpu.pc == RAM_BASE + 0x100


def test_jal_negative_offset(cpu):
    load(cpu, [jal(0, -4)], base=RAM_BASE + 0x100)
    cpu.pc = RAM_BASE + 0x100
    cpu.step()
    assert cpu.pc == RAM_BASE + 0x100 - 4


def test_jalr_clears_low_bit(cpu):
    cpu.write_reg(2, RAM_BASE + 0x201)  # odd address
    load(cpu, [jalr(1, 2, 0)])
    cpu.step()
    assert cpu.pc == RAM_BASE + 0x200  # low bit cleared
    assert cpu.read_reg(1) == RAM_BASE + 4


def test_jalr_rd_equals_rs1_uses_old_value(cpu):
    cpu.write_reg(1, RAM_BASE + 0x40)
    load(cpu, [jalr(1, 1, 4)])
    cpu.step()
    assert cpu.pc == RAM_BASE + 0x44
    assert cpu.read_reg(1) == RAM_BASE + 4


def test_branches_taken_and_not_taken(cpu):
    cases = [
        (beq, 5, 5, True),
        (beq, 5, 6, False),
        (bne, 5, 6, True),
        (bne, 5, 5, False),
        (blt, -1, 1, True),  # signed: -1 < 1
        (blt, 1, -1, False),
        (bge, 1, -1, True),
        (bltu, 0xFFFFFFFF, 1, False),  # unsigned: huge value is not < 1
        (bgeu, 0xFFFFFFFF, 1, True),
    ]
    for enc, a, b, expect_taken in cases:
        cpu.reset()
        cpu.write_reg(1, a & 0xFFFFFFFF)
        cpu.write_reg(2, b & 0xFFFFFFFF)
        load(cpu, [enc(1, 2, 0x20)])
        cpu.step()
        expected_pc = RAM_BASE + (0x20 if expect_taken else 4)
        assert cpu.pc == expected_pc, f"{enc.__name__}({a}, {b})"


def test_loads_sign_and_zero_extend(cpu):
    cpu.memory.write(RAM_BASE + 0x40, 1, 0xFF)  # -1 as a byte
    cpu.memory.write(RAM_BASE + 0x44, 2, 0xFFFF)  # -1 as a halfword
    cpu.write_reg(1, RAM_BASE)

    load(cpu, [lb(2, 1, 0x40)])
    cpu.step()
    assert cpu.read_reg(2) == 0xFFFFFFFF  # sign-extended

    load(cpu, [lbu(2, 1, 0x40)])
    cpu.step()
    assert cpu.read_reg(2) == 0x000000FF  # zero-extended

    load(cpu, [lh(2, 1, 0x44)])
    cpu.step()
    assert cpu.read_reg(2) == 0xFFFFFFFF

    load(cpu, [lhu(2, 1, 0x44)])
    cpu.step()
    assert cpu.read_reg(2) == 0x0000FFFF


def test_load_word_roundtrip(cpu):
    cpu.memory.write(RAM_BASE + 0x40, 4, 0xDEADBEEF)
    cpu.write_reg(1, RAM_BASE)
    load(cpu, [lw(2, 1, 0x40)])
    cpu.step()
    assert cpu.read_reg(2) == 0xDEADBEEF


def test_stores_roundtrip(cpu):
    cpu.write_reg(1, RAM_BASE)
    cpu.write_reg(2, 0xAABBCCDD)

    load(cpu, [sb(1, 2, 0x40)])
    cpu.step()
    assert cpu.memory.read(RAM_BASE + 0x40, 1) == 0xDD

    load(cpu, [sh(1, 2, 0x44)])
    cpu.step()
    assert cpu.memory.read(RAM_BASE + 0x44, 2) == 0xCCDD

    load(cpu, [sw(1, 2, 0x48)])
    cpu.step()
    assert cpu.memory.read(RAM_BASE + 0x48, 4) == 0xAABBCCDD


def test_alu_imm_ops(cpu):
    cpu.write_reg(1, 10)
    load(cpu, [addi(2, 1, -3)])
    cpu.step()
    assert cpu.read_reg(2) == 7

    cpu.write_reg(1, 0xFFFFFFFF)  # -1
    load(cpu, [slti(2, 1, 0)])
    cpu.step()
    assert cpu.read_reg(2) == 1  # -1 < 0 signed

    load(cpu, [sltiu(2, 1, 0)])
    cpu.step()
    assert cpu.read_reg(2) == 0  # 0xFFFFFFFF is not < 0 unsigned

    cpu.write_reg(1, 0b1010)
    load(cpu, [xori(2, 1, 0b0110)])
    cpu.step()
    assert cpu.read_reg(2) == 0b1100

    load(cpu, [ori(2, 1, 0b0110)])
    cpu.step()
    assert cpu.read_reg(2) == 0b1110

    load(cpu, [andi(2, 1, 0b0110)])
    cpu.step()
    assert cpu.read_reg(2) == 0b0010


def test_shifts(cpu):
    cpu.write_reg(1, 1)
    load(cpu, [slli(2, 1, 4)])
    cpu.step()
    assert cpu.read_reg(2) == 16

    cpu.write_reg(1, 0x8000_0000)
    load(cpu, [srli(2, 1, 4)])
    cpu.step()
    assert cpu.read_reg(2) == 0x0800_0000  # logical: zero-filled

    load(cpu, [srai(2, 1, 4)])
    cpu.step()
    assert cpu.read_reg(2) == 0xF800_0000  # arithmetic: sign-filled


def test_alu_reg_ops(cpu):
    cpu.write_reg(1, 5)
    cpu.write_reg(2, 3)
    load(cpu, [add(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 8

    load(cpu, [sub(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 2

    cpu.write_reg(1, 0)
    load(cpu, [sub(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0xFFFFFFFD  # wraps

    cpu.write_reg(1, 0xFFFFFFFF)  # -1
    cpu.write_reg(2, 0)
    load(cpu, [slt(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 1  # signed: -1 < 0

    load(cpu, [sltu(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0  # unsigned: huge value is not < 0

    cpu.write_reg(1, 0b1010)
    cpu.write_reg(2, 0b0110)
    load(cpu, [xor_(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0b1100
    load(cpu, [or_(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0b1110
    load(cpu, [and_(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0b0010

    cpu.write_reg(1, 0x8000_0000)
    cpu.write_reg(2, 4)
    load(cpu, [srl(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0x0800_0000
    load(cpu, [sra(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0xF800_0000
    load(cpu, [sll(3, 1, 2)])
    cpu.step()
    assert cpu.read_reg(3) == 0x8000_0000 << 4 & 0xFFFFFFFF


def test_fence_is_noop(cpu):
    from .asm import fence

    load(cpu, [fence()])
    cpu.step()
    assert cpu.pc == RAM_BASE + 4
    assert cpu.icount == 1


def test_run_stops_at_halt(cpu):
    load(cpu, [addi(1, 0, 1), addi(1, 1, 1), ecall()])
    halt = cpu.run(max_instructions=10)
    assert halt.reason == "ECALL"
    assert cpu.read_reg(1) == 2
    assert cpu.icount == 2
