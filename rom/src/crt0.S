# crt0.S — bare-metal entry point (SPEC §1, §2).
#
# Reset state per SPEC §1: pc = 0x8000_0000, x1..x31 = 0. That's the CPU's
# job (the emulator/refemu), not this file's. crt0's job is exactly the
# three things SPEC §1 names: set sp, zero .bss, jump to main.
#
# No gp setup: rom/Makefile passes -mno-relax, so the toolchain never emits
# gp-relative addressing (the linker-relaxation pass that needs it is
# disabled outright), which means gp is simply never read. Simpler than
# maintaining a __global_pointer$ dance for an optimization we don't need
# against a from-scratch SQL CPU that has to pre-decode every instruction
# word up front (ADR-0002) — fewer addressing modes in play is a feature
# here, not just a simplification.

    # Names this source in the linked ELF's symbol table. Without it the
    # linker names this object's local symbols after the object file, and
    # rom/Makefile compiles and links in one step, so that name is a
    # temporary gcc picks at random.
    .file "crt0.S"

    .section .text.crt0, "ax", @progbits
    .global _start
    .type _start, @function
_start:
    la   sp, __stack_top

    # Zero .bss: __bss_start/__bss_end come from toolchain/link.ld. Loop
    # condition first so an empty .bss (start == end) does zero stores.
    la   a0, __bss_start
    la   a1, __bss_end
    j    2f
1:
    sw   zero, 0(a0)
    addi a0, a0, 4
2:
    bltu a0, a1, 1b

    call main

    # main has nowhere to return to — there is no OS here. Spin rather than
    # fall through into whatever bytes happen to follow in RAM.
.Lhang:
    j .Lhang

    .size _start, . - _start
