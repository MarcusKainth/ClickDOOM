"""RV32I interpreter core (SPEC §1).

This is the oracle: `sqlcpu` is checked against it instruction by
instruction, so clarity beats cleverness beats speed. Every opcode arm is a
direct transcription of the RISC-V base ISA's instruction formats — if it
doesn't look obviously correct on read, it isn't done.

Two things this file deliberately leaves to sibling issues, with the seam
marked at the point where they plug in:
- The M-extension (mul/div/rem, opcode 0x33 funct7=0x01) is issue #12.
  Until it lands, those encodings decode as ILLEGAL_INSN like any other
  unimplemented opcode — correct behavior for an RV32I-only test, and not a
  divergence once #12 fills the arm in.
- MMIO register semantics (TICKS_MS, KEYQ, EXIT, PUTCHAR, FRAME_COMMIT) are
  issue #13's; see `memory.NullMmio`.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from .memory import RAM_BASE, BadAddr, Memory, Misaligned, SelfModify


class HaltReason:
    """Halt reason codes (SPEC §1: "fatal halt with reason code").

    Only ILLEGAL_INSN, BAD_ADDR and SELF_MODIFY are spelled out by name in
    SPEC §1; misaligned access and ecall/ebreak/CSR are "fatal halt with
    reason code" too, but SPEC does not mandate a string for them. The
    names below are refemu's choice. They matter beyond this file only if
    `sqlcpu` wants byte-identical `cpu_state.halt_reason` values (SPEC §5)
    — coordinate before relying on the exact spelling.
    """

    ILLEGAL_INSN = "ILLEGAL_INSN"
    BAD_ADDR = "BAD_ADDR"
    SELF_MODIFY = "SELF_MODIFY"
    MISALIGNED = "MISALIGNED"
    ECALL = "ECALL"
    EBREAK = "EBREAK"
    CSR = "CSR"


class Halted(Exception):
    """Raised by `CPU.step()` on any fatal halt (SPEC §1). Carries
    everything the halt record needs: reason, pc, and, when applicable,
    the raw instruction word (ILLEGAL_INSN, SELF_MODIFY) or the faulting
    address (BAD_ADDR, MISALIGNED, SELF_MODIFY)."""

    def __init__(self, reason: str, pc: int, insn: int | None = None, addr: int | None = None):
        self.reason = reason
        self.pc = pc
        self.insn = insn
        self.addr = addr
        super().__init__(f"{reason} at pc=0x{pc:08x}")


def _sext(value: int, bits: int) -> int:
    """Sign-extend the low `bits` bits of `value` to a Python int."""
    sign = 1 << (bits - 1)
    return (value & (sign - 1)) - (value & sign)


def _u32(value: int) -> int:
    return value & 0xFFFF_FFFF


def _s32(value: int) -> int:
    value &= 0xFFFF_FFFF
    return value - 0x1_0000_0000 if value & 0x8000_0000 else value


# Opcodes (instruction bits [6:0]).
OP_LUI = 0x37
OP_AUIPC = 0x17
OP_JAL = 0x6F
OP_JALR = 0x67
OP_BRANCH = 0x63
OP_LOAD = 0x03
OP_STORE = 0x23
OP_IMM = 0x13
OP_REG = 0x33
OP_FENCE = 0x0F
OP_SYSTEM = 0x73


@dataclass
class CPU:
    memory: Memory
    pc: int = RAM_BASE
    regs: list[int] = field(default_factory=lambda: [0] * 32)
    icount: int = 0

    def reset(self, pc: int = RAM_BASE) -> None:
        """SPEC §1 reset state: pc at RAM base, x1..x31 zeroed (crt0 sets
        sp and jumps to main from there — not this class's job)."""
        self.pc = pc
        self.regs = [0] * 32
        self.icount = 0

    # -- register file: x0 hardwired to 0 (SPEC §1) --

    def read_reg(self, i: int) -> int:
        return self.regs[i]

    def write_reg(self, i: int, value: int) -> None:
        if i != 0:
            self.regs[i] = _u32(value)

    # -- execution --

    def step(self) -> None:
        """Fetch, decode and execute one instruction. Raises `Halted` on
        any SPEC §1 fatal condition; otherwise advances pc and icount."""
        pc = self.pc
        try:
            insn = self.memory.read(pc, 4)
        except BadAddr as e:
            raise Halted(HaltReason.BAD_ADDR, pc, addr=e.addr) from None
        except Misaligned as e:
            raise Halted(HaltReason.MISALIGNED, pc, addr=e.addr) from None

        next_pc = self._execute(pc, insn)
        self.icount += 1
        self.pc = next_pc

    def run(self, max_instructions: int | None = None) -> Halted:
        """Step until halt. `max_instructions` is a test/harness safety
        valve, not a SPEC concept — exceeding it is a hung-program bug in
        whatever's being run, so it raises rather than returning quietly."""
        count = 0
        while True:
            if max_instructions is not None and count >= max_instructions:
                raise RuntimeError(f"did not halt within {max_instructions} instructions")
            try:
                self.step()
            except Halted as halt:
                return halt
            count += 1

    def _execute(self, pc: int, insn: int) -> int:
        """Decode and execute `insn` fetched from `pc`; return the next pc.

        Jump/branch targets are *not* alignment-checked here: SPEC only
        requires misaligned word/halfword *access* to fault, and the real
        RISC-V behavior this mirrors is that a jump architecturally
        completes (pc and rd both update) and it is the next instruction
        *fetch* that faults if the target is misaligned or out of range —
        `step()`'s fetch already does that check on the following call.
        """
        opcode = insn & 0x7F
        rd = (insn >> 7) & 0x1F
        funct3 = (insn >> 12) & 0x7
        rs1 = (insn >> 15) & 0x1F
        rs2 = (insn >> 20) & 0x1F
        funct7 = (insn >> 25) & 0x7F

        if opcode == OP_LUI:
            self.write_reg(rd, insn & 0xFFFF_F000)
            return pc + 4

        if opcode == OP_AUIPC:
            self.write_reg(rd, pc + (insn & 0xFFFF_F000))
            return pc + 4

        if opcode == OP_JAL:
            imm = _sext(
                (((insn >> 31) & 0x1) << 20)
                | (((insn >> 12) & 0xFF) << 12)
                | (((insn >> 20) & 0x1) << 11)
                | (((insn >> 21) & 0x3FF) << 1),
                21,
            )
            target = _u32(pc + imm)
            self.write_reg(rd, pc + 4)
            return target

        if opcode == OP_JALR:
            if funct3 != 0b000:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            imm = _sext(insn >> 20, 12)
            target = _u32((self.read_reg(rs1) + imm) & ~1)
            self.write_reg(rd, pc + 4)
            return target

        if opcode == OP_BRANCH:
            imm = _sext(
                (((insn >> 31) & 0x1) << 12)
                | (((insn >> 7) & 0x1) << 11)
                | (((insn >> 25) & 0x3F) << 5)
                | (((insn >> 8) & 0xF) << 1),
                13,
            )
            a, b = self.read_reg(rs1), self.read_reg(rs2)
            taken = {
                0b000: _s32(a) == _s32(b),  # beq
                0b001: _s32(a) != _s32(b),  # bne
                0b100: _s32(a) < _s32(b),  # blt
                0b101: _s32(a) >= _s32(b),  # bge
                0b110: a < b,  # bltu (already unsigned)
                0b111: a >= b,  # bgeu
            }.get(funct3)
            if taken is None:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            return _u32(pc + imm) if taken else pc + 4

        if opcode == OP_LOAD:
            imm = _sext(insn >> 20, 12)
            addr = _u32(self.read_reg(rs1) + imm)
            width, signed = {
                0b000: (1, True),  # lb
                0b001: (2, True),  # lh
                0b010: (4, True),  # lw
                0b100: (1, False),  # lbu
                0b101: (2, False),  # lhu
            }.get(funct3, (None, None))
            if width is None:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            try:
                raw = self.memory.read(addr, width)
            except BadAddr as e:
                raise Halted(HaltReason.BAD_ADDR, pc, insn=insn, addr=e.addr) from None
            except Misaligned as e:
                raise Halted(HaltReason.MISALIGNED, pc, insn=insn, addr=e.addr) from None
            value = _sext(raw, width * 8) if signed else raw
            self.write_reg(rd, value)
            return pc + 4

        if opcode == OP_STORE:
            imm = _sext(((insn >> 25) << 5) | ((insn >> 7) & 0x1F), 12)
            addr = _u32(self.read_reg(rs1) + imm)
            width = {0b000: 1, 0b001: 2, 0b010: 4}.get(funct3)  # sb / sh / sw
            if width is None:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            value = self.read_reg(rs2)
            try:
                self.memory.write(addr, width, value)
            except BadAddr as e:
                raise Halted(HaltReason.BAD_ADDR, pc, insn=insn, addr=e.addr) from None
            except Misaligned as e:
                raise Halted(HaltReason.MISALIGNED, pc, insn=insn, addr=e.addr) from None
            except SelfModify as e:
                raise Halted(HaltReason.SELF_MODIFY, pc, insn=insn, addr=e.addr) from None
            return pc + 4

        if opcode == OP_IMM:
            imm = _sext(insn >> 20, 12)
            a = self.read_reg(rs1)
            shamt = imm & 0x1F
            if funct3 == 0b000:
                result = _u32(a + imm)  # addi
            elif funct3 == 0b010:
                result = int(_s32(a) < imm)  # slti
            elif funct3 == 0b011:
                result = int(a < _u32(imm))  # sltiu
            elif funct3 == 0b100:
                result = a ^ _u32(imm)  # xori
            elif funct3 == 0b110:
                result = a | _u32(imm)  # ori
            elif funct3 == 0b111:
                result = a & _u32(imm)  # andi
            elif funct3 == 0b001 and funct7 == 0x00:
                result = _u32(a << shamt)  # slli
            elif funct3 == 0b101 and funct7 == 0x00:
                result = a >> shamt  # srli
            elif funct3 == 0b101 and funct7 == 0x20:
                result = _u32(_s32(a) >> shamt)  # srai
            else:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            self.write_reg(rd, result)
            return pc + 4

        if opcode == OP_REG:
            if funct7 == 0x01:
                # M-extension (mul/mulh/mulhsu/mulhu/div/divu/rem/remu):
                # issue #12 fills this arm in.
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            a, b = self.read_reg(rs1), self.read_reg(rs2)
            shamt = b & 0x1F
            ops = {
                (0b000, 0x00): lambda: _u32(a + b),  # add
                (0b000, 0x20): lambda: _u32(a - b),  # sub
                (0b001, 0x00): lambda: _u32(a << shamt),  # sll
                (0b010, 0x00): lambda: int(_s32(a) < _s32(b)),  # slt
                (0b011, 0x00): lambda: int(a < b),  # sltu
                (0b100, 0x00): lambda: a ^ b,  # xor
                (0b101, 0x00): lambda: a >> shamt,  # srl
                (0b101, 0x20): lambda: _u32(_s32(a) >> shamt),  # sra
                (0b110, 0x00): lambda: a | b,  # or
                (0b111, 0x00): lambda: a & b,  # and
            }.get((funct3, funct7))
            if ops is None:
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            self.write_reg(rd, ops())
            return pc + 4

        if opcode == OP_FENCE:
            # FENCE / FENCE.I: single hart, no cache, nothing to reorder
            # against — a no-op. Unlike ecall/ebreak/CSR, SPEC does not
            # list this as unexpected; toolchain-emitted fences should not
            # halt the machine.
            return pc + 4

        if opcode == OP_SYSTEM:
            if funct3 == 0b000:
                imm12 = insn >> 20
                if imm12 == 0:
                    raise Halted(HaltReason.ECALL, pc, insn=insn)
                if imm12 == 1:
                    raise Halted(HaltReason.EBREAK, pc, insn=insn)
                raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
            if funct3 in (0b001, 0b010, 0b011, 0b101, 0b110, 0b111):
                # csrrw/csrrs/csrrc/csrrwi/csrrsi/csrrci
                raise Halted(HaltReason.CSR, pc, insn=insn)
            raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)

        raise Halted(HaltReason.ILLEGAL_INSN, pc, insn=insn)
