# Placeholder ROM entry point for the toolchain smoke test (issue #5).
#
# This is NOT crt0. It exists only so the pinned toolchain and a linker
# script have something real to build, proving `just build-rom` produces a
# byte-reproducible rom/build/doom-rv32im.bin from a clean checkout and that
# CI's build-rom job (gated on rom/Makefile existing) goes green. Issue #6
# replaces this file and toolchain/placeholder.ld with the real crt0, linker
# script and SPEC §2 memory map (text/rodata/data/bss/heap/stack regions,
# text made read-only per ADR-0002).
#
# SPEC §1: RV32IM, no compressed extension, no ecall/ebreak/CSR, reset
# pc = 0x8000_0000. This stub trivially satisfies all of that — it is one
# instruction, no CSRs, no compressed forms, and sits at the reset address
# via toolchain/placeholder.ld.

    .section .text
    .globl _start
_start:
    j _start
