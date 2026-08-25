"""M-extension tests (issue #12). The edge cases are the whole job:
division by zero (no trap), INT_MIN overflow, and mulhsu's mixed
operand signedness -- exactly what would silently diverge from sqlcpu
if either implementation got sloppy.
"""

from .asm import div, divu, mul, mulh, mulhsu, mulhu, rem, remu
from .conftest import load

U32 = 0xFFFF_FFFF
INT_MIN = 0x8000_0000  # -2147483648 as an unsigned bit pattern
INT_MAX = 0x7FFF_FFFF


def _run(cpu, enc, a, b):
    cpu.write_reg(1, a & U32)
    cpu.write_reg(2, b & U32)
    load(cpu, [enc(3, 1, 2)])
    cpu.step()
    return cpu.read_reg(3)


def test_mul_low_bits_wrap(cpu):
    assert _run(cpu, mul, 6, 7) == 42
    # 0x10000 * 0x10000 overflows 32 bits; mul keeps only the low word.
    assert _run(cpu, mul, 0x10000, 0x10000) == 0


def test_mul_negative_operands(cpu):
    assert _run(cpu, mul, -6, 7) == (-42) & U32


def test_mulh_signed_times_signed(cpu):
    # -1 * -1 = 1; high word of a 64-bit 1 is 0.
    assert _run(cpu, mulh, -1, -1) == 0
    # INT_MIN * INT_MIN = 2**62, whose high 32 bits are 0x4000_0000.
    assert _run(cpu, mulh, INT_MIN, INT_MIN) == 0x4000_0000


def test_mulhsu_operand_signedness(cpu):
    # rs1 signed, rs2 unsigned: -1 (signed) x 2 (unsigned) = -2, whose
    # 64-bit two's-complement high word is all ones. Getting the operand
    # signedness backwards (treating rs2 as signed, or rs1 as unsigned)
    # gives a different answer here, which is the point of the test.
    assert _run(cpu, mulhsu, -1, 2) == U32
    # rs1 = 0xFFFFFFFF unsigned would be -1 signed x rs2 = 0xFFFFFFFF
    # (unsigned, i.e. 4294967295): product = -4294967295, top word 0xFFFFFFFF.
    assert _run(cpu, mulhsu, -1, U32) == U32


def test_mulhu_unsigned_times_unsigned(cpu):
    # 0xFFFFFFFF * 0xFFFFFFFF = 0xFFFFFFFE00000001; high word 0xFFFFFFFE.
    assert _run(cpu, mulhu, U32, U32) == 0xFFFF_FFFE


def test_div_truncates_toward_zero(cpu):
    assert _run(cpu, div, 7, 2) == 3
    assert _run(cpu, div, -7, 2) == (-3) & U32  # not -4 (floor)
    assert _run(cpu, div, 7, -2) == (-3) & U32
    assert _run(cpu, div, -7, -2) == 3


def test_div_by_zero_returns_all_ones_no_trap(cpu):
    assert _run(cpu, div, 5, 0) == U32
    assert _run(cpu, div, -5, 0) == U32


def test_div_overflow_returns_dividend(cpu):
    # INT_MIN / -1 overflows a 32-bit signed result; RISC-V defines this
    # as returning INT_MIN rather than trapping.
    assert _run(cpu, div, INT_MIN, -1) == INT_MIN


def test_divu_by_zero_returns_all_ones_no_trap(cpu):
    assert _run(cpu, divu, 5, 0) == U32


def test_divu_treats_operands_as_unsigned(cpu):
    # 0xFFFFFFFF (huge unsigned) / 2, not (-1 signed) / 2.
    assert _run(cpu, divu, U32, 2) == 0x7FFF_FFFF


def test_rem_sign_matches_dividend(cpu):
    assert _run(cpu, rem, 7, 2) == 1
    assert _run(cpu, rem, -7, 2) == (-1) & U32
    assert _run(cpu, rem, 7, -2) == 1
    assert _run(cpu, rem, -7, -2) == (-1) & U32


def test_rem_by_zero_returns_dividend_no_trap(cpu):
    assert _run(cpu, rem, 42, 0) == 42
    assert _run(cpu, rem, -42, 0) == (-42) & U32


def test_rem_overflow_returns_zero(cpu):
    assert _run(cpu, rem, INT_MIN, -1) == 0


def test_remu_by_zero_returns_dividend_no_trap(cpu):
    assert _run(cpu, remu, U32, 0) == U32


def test_remu_treats_operands_as_unsigned(cpu):
    assert _run(cpu, remu, U32, 2) == 1  # 4294967295 % 2
