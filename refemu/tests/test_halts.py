"""Every SPEC §1 halt reason, reachable and asserted on. This is the
"done when" for issue #11: ILLEGAL_INSN, BAD_ADDR, SELF_MODIFY, misaligned
access, and ecall/ebreak/CSR all halt, carry the right halt-record fields,
and leave no partial side effect behind."""

import pytest

from refemu.cpu import Halted, HaltReason
from refemu.memory import (
    FRAMEBUFFER_BASE,
    MMIO_BASE,
    PALETTE_BASE,
    RAM_BASE,
    RAM_SIZE,
    Memory,
)

from .asm import (
    RESERVED_OPCODE,
    addi,
    csrrw,
    ebreak,
    ecall,
    jal,
    lw,
    sw,
)
from .conftest import load


def test_illegal_insn_reserved_opcode(cpu):
    load(cpu, [RESERVED_OPCODE])
    with pytest.raises(Halted) as exc:
        cpu.step()
    halt = exc.value
    assert halt.reason == HaltReason.ILLEGAL_INSN
    assert halt.pc == RAM_BASE
    assert halt.insn == RESERVED_OPCODE
    # No partial effect: pc/icount unchanged.
    assert cpu.pc == RAM_BASE
    assert cpu.icount == 0


def test_illegal_insn_bad_alu_funct(cpu):
    # opcode=OP_REG, funct3=funct7=0 is 'add' (legal); flip funct7 to an
    # unassigned value (not 0x00, 0x01=M-ext, or 0x20) to hit the arm's
    # else-branch.
    bad = (0x02 << 25) | (0 << 20) | (0 << 15) | (0b000 << 12) | (1 << 7) | 0x33
    load(cpu, [bad])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.ILLEGAL_INSN


def test_illegal_insn_unimplemented_m_extension(cpu):
    # funct7=0x01 on OP_REG is the M-extension seam issue #12 fills in;
    # until then it must halt cleanly, not silently compute garbage.
    from .asm import mul

    load(cpu, [mul(1, 0, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.ILLEGAL_INSN


def test_bad_addr_load_outside_all_regions(cpu):
    cpu.write_reg(1, 0)  # address 0 is in none of SPEC §2's four regions
    load(cpu, [lw(2, 1, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    halt = exc.value
    assert halt.reason == HaltReason.BAD_ADDR
    assert halt.addr == 0
    assert halt.pc == RAM_BASE
    assert cpu.read_reg(2) == 0  # load never landed


def test_bad_addr_store_outside_all_regions(cpu):
    cpu.write_reg(1, 0xFFFFFFF0)  # past every region, near the top of the space
    cpu.write_reg(2, 0xDEADBEEF)
    load(cpu, [sw(1, 2, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.BAD_ADDR


def test_bad_addr_just_past_ram(cpu):
    addr = RAM_BASE + RAM_SIZE  # one byte past the last valid RAM word
    cpu.write_reg(1, addr)
    load(cpu, [lw(2, 1, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.BAD_ADDR


def test_bad_addr_instruction_fetch(cpu):
    cpu.pc = 0  # fetch itself is a memory access subject to §2
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.BAD_ADDR
    assert exc.value.addr == 0


@pytest.mark.parametrize("region_base", [MMIO_BASE, FRAMEBUFFER_BASE, PALETTE_BASE])
def test_non_ram_regions_are_not_bad_addr(cpu, region_base):
    # MMIO/FRAMEBUFFER/PALETTE are valid per SPEC §2, even though real MMIO
    # *semantics* land in issue #13 — a plain load/store there must not
    # fault the machine.
    cpu.write_reg(1, region_base)
    cpu.write_reg(2, 0x11223344)
    load(cpu, [sw(1, 2, 0)])
    cpu.step()  # must not raise
    load(cpu, [lw(3, 1, 0)])
    cpu.step()
    assert cpu.read_reg(3) == 0x11223344


def test_misaligned_word_load(cpu):
    cpu.write_reg(1, RAM_BASE + 1)
    load(cpu, [lw(2, 1, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    halt = exc.value
    assert halt.reason == HaltReason.MISALIGNED
    assert halt.addr == RAM_BASE + 1


def test_misaligned_halfword_store(cpu):
    from .asm import sh

    cpu.write_reg(1, RAM_BASE + 1)
    cpu.write_reg(2, 0xBEEF)
    load(cpu, [sh(1, 2, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.MISALIGNED


def test_byte_access_never_misaligned(cpu):
    # Bytes have no alignment requirement at any offset. Target address is
    # well clear of the single-instruction fixtures `load()` below writes
    # at RAM_BASE, so the two don't overlap in memory.
    from .asm import lb, sb

    cpu.write_reg(1, RAM_BASE + 0x41)  # odd, unaligned to any width
    cpu.write_reg(2, 0xAB)
    load(cpu, [sb(1, 2, 0)])
    cpu.step()
    load(cpu, [lb(3, 1, 0)])
    cpu.step()
    assert cpu.read_reg(3) == 0xFFFFFFAB  # sign-extended 0xAB, not a fault


def test_misaligned_fetch_after_jal(cpu):
    # jal architecturally completes (rd is written, pc updates) even to a
    # misaligned target; the fault surfaces on the *next* fetch, matching
    # real RISC-V's instruction-address-misaligned semantics.
    load(cpu, [jal(1, 2)])  # target = RAM_BASE + 2, not 4-aligned
    cpu.step()
    assert cpu.read_reg(1) == RAM_BASE + 4  # link register did get written
    assert cpu.pc == RAM_BASE + 2
    with pytest.raises(Halted) as exc:
        cpu.step()
    halt = exc.value
    assert halt.reason == HaltReason.MISALIGNED
    assert halt.pc == RAM_BASE + 2


def test_self_modify_halts_on_text_store(cpu):
    mem = Memory(text_start=RAM_BASE, text_end=RAM_BASE + 0x1000)
    cpu = cpu.__class__(memory=mem)
    cpu.write_reg(1, RAM_BASE + 0x40)
    cpu.write_reg(2, 0)
    load(cpu, [sw(1, 2, 0)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    halt = exc.value
    assert halt.reason == HaltReason.SELF_MODIFY
    assert halt.addr == RAM_BASE + 0x40
    assert halt.pc == RAM_BASE  # the store instruction itself lives outside text here


def test_self_modify_boundary_is_exclusive(cpu):
    # [text_start, text_end): a store exactly at text_end is outside text.
    mem = Memory(text_start=RAM_BASE, text_end=RAM_BASE + 0x40)
    cpu = cpu.__class__(memory=mem)
    cpu.write_reg(1, RAM_BASE + 0x40)
    cpu.write_reg(2, 0)
    load(cpu, [sw(1, 2, 0)], base=RAM_BASE + 0x100)
    cpu.step()  # must not raise: 0x40 is text_end, not inside [start, end)


def test_no_text_region_disables_self_modify(cpu):
    # riscv-tests have no ROM manifest / text region at all.
    assert cpu.memory.text_start is None
    cpu.write_reg(1, RAM_BASE + 0x40)
    load(cpu, [sw(1, 1, 0)])
    cpu.step()  # must not raise


def test_ecall_halts(cpu):
    load(cpu, [ecall()])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.ECALL
    assert exc.value.pc == RAM_BASE


def test_ebreak_halts(cpu):
    load(cpu, [ebreak()])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.EBREAK


def test_csr_halts(cpu):
    load(cpu, [csrrw(1, 2, 0x300)])
    with pytest.raises(Halted) as exc:
        cpu.step()
    assert exc.value.reason == HaltReason.CSR


def test_halt_does_not_advance_pc_or_icount(cpu):
    load(cpu, [addi(1, 0, 1), RESERVED_OPCODE])
    cpu.step()
    assert cpu.icount == 1
    with pytest.raises(Halted):
        cpu.step()
    assert cpu.icount == 1
    assert cpu.pc == RAM_BASE + 4  # still pointing at the faulting instruction
