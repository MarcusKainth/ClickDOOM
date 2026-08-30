"""Official riscv-tests rv32ui/rv32um suites, run against the refemu core
(issue #14). Fixtures are pre-built flat binaries in
tests/fixtures/riscv_tests/ (see scripts/build_riscv_tests.sh and
riscv-tests-env/ for how and why they're built against a custom minimal
environment rather than upstream's env/p).

Two upstream rv32ui tests are deliberately absent from the fixture set --
not skipped here, never generated in the first place -- because they
require CPU behavior SPEC.md explicitly rules out:
  fence_i -- self-modifying code; SPEC §1/ADR-0002 make a text-region
             store a fatal SELF_MODIFY halt.
  ma_data -- transparent misaligned load/store; SPEC §1 makes misaligned
             access a fatal halt.
See scripts/build_riscv_tests.sh for the exclusion list and rationale.

Each fixture is its own parametrized pytest case, so `make test-refemu`'s
plain `pytest -q` summary line ("N passed") is the pass count issue #14
asks for -- no separate counting mechanism to keep in sync.
"""

from pathlib import Path

import pytest

from refemu.cpu import CPU, HaltReason
from refemu.memory import RAM_BASE, Memory

FIXTURES_DIR = Path(__file__).parent / "fixtures" / "riscv_tests"
FIXTURES = sorted(FIXTURES_DIR.glob("*.bin"))


@pytest.mark.parametrize("fixture", FIXTURES, ids=lambda p: p.stem)
def test_riscv_test(fixture: Path):
    cpu = CPU(memory=Memory())
    cpu.memory.load_image(fixture.read_bytes(), base=RAM_BASE)
    cpu.pc = RAM_BASE

    halt = cpu.run(max_instructions=200_000)

    assert halt.reason == HaltReason.ECALL, (
        f"{fixture.stem}: expected a clean ECALL exit (riscv-tests' pass/fail "
        f"signal), got {halt.reason} at pc=0x{halt.pc:08x} (icount={cpu.icount})"
    )
    exit_code = cpu.read_reg(10)  # a0, riscv-tests' exit-syscall convention
    if exit_code != 0:
        # riscv-tests' RVTEST_FAIL encodes the failing test number as
        # (gp << 1) | 1 before writing it to a0; invert that for a useful
        # failure message (env/p/riscv_test.h, RVTEST_FAIL).
        failing_testnum = (exit_code - 1) >> 1
        pytest.fail(
            f"{fixture.stem}: test case {failing_testnum} failed "
            f"(a0=0x{exit_code:x}, icount={cpu.icount})"
        )


def test_fixtures_present():
    # Guards against an empty glob (e.g. a path typo) silently reporting
    # "0 passed" as a green run.
    assert len(FIXTURES) == 48, (
        f"expected 48 riscv-tests fixtures (40 rv32ui + 8 rv32um), found "
        f"{len(FIXTURES)} -- did scripts/build_riscv_tests.sh run, or did "
        f"the fixtures directory move?"
    )
