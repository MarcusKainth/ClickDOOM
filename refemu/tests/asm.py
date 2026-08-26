"""Raw RV32I instruction-word encoders, for tests only.

Deliberately dependency-free (no assembler, no external toolchain): each
encoder is a direct transcription of the RISC-V base ISA's instruction
formats, so a reader can check a test's encoding against the ISA manual
line by line.
"""

from __future__ import annotations


def _u(value: int, bits: int) -> int:
    return value & ((1 << bits) - 1)


def r_type(opcode: int, rd: int, funct3: int, rs1: int, rs2: int, funct7: int) -> int:
    return (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode


def i_type(opcode: int, rd: int, funct3: int, rs1: int, imm: int) -> int:
    return (_u(imm, 12) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode


def s_type(opcode: int, funct3: int, rs1: int, rs2: int, imm: int) -> int:
    imm = _u(imm, 12)
    return (
        ((imm >> 5) << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | ((imm & 0x1F) << 7) | opcode
    )


def b_type(opcode: int, funct3: int, rs1: int, rs2: int, imm: int) -> int:
    imm = _u(imm, 13)  # bit 0 is always 0 (branch offsets are even)
    bit12 = (imm >> 12) & 1
    bit11 = (imm >> 11) & 1
    bits10_5 = (imm >> 5) & 0x3F
    bits4_1 = (imm >> 1) & 0xF
    return (
        (bit12 << 31)
        | (bits10_5 << 25)
        | (rs2 << 20)
        | (rs1 << 15)
        | (funct3 << 12)
        | (bits4_1 << 8)
        | (bit11 << 7)
        | opcode
    )


def u_type(opcode: int, rd: int, imm20: int) -> int:
    return (_u(imm20, 20) << 12) | (rd << 7) | opcode


def j_type(opcode: int, rd: int, imm: int) -> int:
    imm = _u(imm, 21)  # bit 0 is always 0
    bit20 = (imm >> 20) & 1
    bits19_12 = (imm >> 12) & 0xFF
    bit11 = (imm >> 11) & 1
    bits10_1 = (imm >> 1) & 0x3FF
    return (bit20 << 31) | (bits10_1 << 21) | (bit11 << 20) | (bits19_12 << 12) | (rd << 7) | opcode


# -- named encoders for the instructions the tests actually use --

def lui(rd, imm20): return u_type(0x37, rd, imm20)
def auipc(rd, imm20): return u_type(0x17, rd, imm20)
def jal(rd, imm): return j_type(0x6F, rd, imm)
def jalr(rd, rs1, imm): return i_type(0x67, rd, 0b000, rs1, imm)

def beq(rs1, rs2, imm): return b_type(0x63, 0b000, rs1, rs2, imm)
def bne(rs1, rs2, imm): return b_type(0x63, 0b001, rs1, rs2, imm)
def blt(rs1, rs2, imm): return b_type(0x63, 0b100, rs1, rs2, imm)
def bge(rs1, rs2, imm): return b_type(0x63, 0b101, rs1, rs2, imm)
def bltu(rs1, rs2, imm): return b_type(0x63, 0b110, rs1, rs2, imm)
def bgeu(rs1, rs2, imm): return b_type(0x63, 0b111, rs1, rs2, imm)

def lb(rd, rs1, imm): return i_type(0x03, rd, 0b000, rs1, imm)
def lh(rd, rs1, imm): return i_type(0x03, rd, 0b001, rs1, imm)
def lw(rd, rs1, imm): return i_type(0x03, rd, 0b010, rs1, imm)
def lbu(rd, rs1, imm): return i_type(0x03, rd, 0b100, rs1, imm)
def lhu(rd, rs1, imm): return i_type(0x03, rd, 0b101, rs1, imm)

def sb(rs1, rs2, imm): return s_type(0x23, 0b000, rs1, rs2, imm)
def sh(rs1, rs2, imm): return s_type(0x23, 0b001, rs1, rs2, imm)
def sw(rs1, rs2, imm): return s_type(0x23, 0b010, rs1, rs2, imm)

def addi(rd, rs1, imm): return i_type(0x13, rd, 0b000, rs1, imm)
def slti(rd, rs1, imm): return i_type(0x13, rd, 0b010, rs1, imm)
def sltiu(rd, rs1, imm): return i_type(0x13, rd, 0b011, rs1, imm)
def xori(rd, rs1, imm): return i_type(0x13, rd, 0b100, rs1, imm)
def ori(rd, rs1, imm): return i_type(0x13, rd, 0b110, rs1, imm)
def andi(rd, rs1, imm): return i_type(0x13, rd, 0b111, rs1, imm)
def slli(rd, rs1, shamt): return r_type(0x13, rd, 0b001, rs1, shamt, 0x00)
def srli(rd, rs1, shamt): return r_type(0x13, rd, 0b101, rs1, shamt, 0x00)
def srai(rd, rs1, shamt): return r_type(0x13, rd, 0b101, rs1, shamt, 0x20)

def add(rd, rs1, rs2): return r_type(0x33, rd, 0b000, rs1, rs2, 0x00)
def sub(rd, rs1, rs2): return r_type(0x33, rd, 0b000, rs1, rs2, 0x20)
def sll(rd, rs1, rs2): return r_type(0x33, rd, 0b001, rs1, rs2, 0x00)
def slt(rd, rs1, rs2): return r_type(0x33, rd, 0b010, rs1, rs2, 0x00)
def sltu(rd, rs1, rs2): return r_type(0x33, rd, 0b011, rs1, rs2, 0x00)
def xor_(rd, rs1, rs2): return r_type(0x33, rd, 0b100, rs1, rs2, 0x00)
def srl(rd, rs1, rs2): return r_type(0x33, rd, 0b101, rs1, rs2, 0x00)
def sra(rd, rs1, rs2): return r_type(0x33, rd, 0b101, rs1, rs2, 0x20)
def or_(rd, rs1, rs2): return r_type(0x33, rd, 0b110, rs1, rs2, 0x00)
def and_(rd, rs1, rs2): return r_type(0x33, rd, 0b111, rs1, rs2, 0x00)

def mul(rd, rs1, rs2): return r_type(0x33, rd, 0b000, rs1, rs2, 0x01)  # M-extension (issue #12)
def mulh(rd, rs1, rs2): return r_type(0x33, rd, 0b001, rs1, rs2, 0x01)
def mulhsu(rd, rs1, rs2): return r_type(0x33, rd, 0b010, rs1, rs2, 0x01)
def mulhu(rd, rs1, rs2): return r_type(0x33, rd, 0b011, rs1, rs2, 0x01)
def div(rd, rs1, rs2): return r_type(0x33, rd, 0b100, rs1, rs2, 0x01)
def divu(rd, rs1, rs2): return r_type(0x33, rd, 0b101, rs1, rs2, 0x01)
def rem(rd, rs1, rs2): return r_type(0x33, rd, 0b110, rs1, rs2, 0x01)
def remu(rd, rs1, rs2): return r_type(0x33, rd, 0b111, rs1, rs2, 0x01)

def fence(): return i_type(0x0F, 0, 0b000, 0, 0)
def ecall(): return i_type(0x73, 0, 0b000, 0, 0)
def ebreak(): return i_type(0x73, 0, 0b000, 0, 1)
def csrrw(rd, rs1, csr): return i_type(0x73, rd, 0b001, rs1, csr)

RESERVED_OPCODE = 0b0000000  # opcode bits all zero: not a valid RV32I opcode
