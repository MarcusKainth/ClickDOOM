// ClickDOOM's minimal "p" (physical/bare-metal) riscv-tests environment.
//
// Why this exists instead of using upstream riscv-tests' env/p/riscv_test.h
// directly: SPEC.md §1 defines ClickDOOM's CPU as "machine-mode only, flat
// physical addressing, no MMU, no interrupts" with "ecall, ebreak, CSR
// instructions: fatal halt" -- there is no CSR file, no mtvec, no PMP, no
// mret. Upstream's env/p reset vector runs INIT_PMP / INIT_SATP /
// DELEGATE_NO_TRAPS / a trap-vector install / `mret` before a single test
// instruction executes -- every one of those is a CSR write, so refemu
// would halt on the very first instruction of every upstream test, before
// the test itself even started. This header replaces that boilerplate
// with nothing: `_start` runs the test body directly, using SPEC §1's
// actual reset state (pc = 0x8000_0000, x1..x31 = 0) instead of
// reconstructing a privileged-mode environment refemu doesn't implement.
//
// RVTEST_PASS/RVTEST_FAIL are unchanged from upstream: both execute
// `ecall` with the RISC-V `exit` syscall convention (a7=93, a0=exit code,
// 0=pass). refemu's harness (tests/test_riscv_tests_suite.py) reads
// exactly that: run to Halted, require reason == "ECALL", then a0 == 0.
//
// This file only needs to satisfy what rv32ui/rv32um test sources actually
// use: RVTEST_RV32U, RVTEST_CODE_BEGIN/END, RVTEST_PASS/FAIL,
// RVTEST_DATA_BEGIN/END. It intentionally does not define RVTEST_RV32M/S,
// RVTEST_RV64*, or anything FP/vector/CSR-related -- ClickDOOM has none of
// those extensions, and nothing in the rv32ui/rv32um suites needs them.

#ifndef _CLICKDOOM_RISCV_TEST_H
#define _CLICKDOOM_RISCV_TEST_H

#define RVTEST_RV32U \
  .macro init;       \
  .endm

#define TESTNUM gp

#define RVTEST_CODE_BEGIN     \
        .section .text.init; \
        .align 2;             \
        .globl _start;        \
_start:                       \
        li TESTNUM, 0;        \
        init;

#define RVTEST_CODE_END unimp

#define RVTEST_PASS      \
        fence;           \
        li TESTNUM, 1;   \
        li a7, 93;       \
        li a0, 0;        \
        ecall

#define RVTEST_FAIL              \
        fence;                   \
1:      beqz TESTNUM, 1b;        \
        sll TESTNUM, TESTNUM, 1; \
        or TESTNUM, TESTNUM, 1;  \
        li a7, 93;               \
        addi a0, TESTNUM, 0;     \
        ecall

#define RVTEST_DATA_BEGIN .align 4; .global begin_signature; begin_signature:
#define RVTEST_DATA_END   .align 4; .global end_signature; end_signature:

#endif
