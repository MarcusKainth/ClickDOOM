"""MMIO device model tests (issue #13, SPEC §3/§3.1/§3.2).

Uses `refemu.cpu.new_cpu()` rather than the bare `cpu` fixture, since that
fixture's `Memory()` defaults to `NullMmio` (plain storage, no register
semantics) -- these tests are specifically about the semantics.
"""

from refemu.cpu import Halted, HaltReason
from refemu.mmio import DEFAULT_IPMS, EXIT, FRAME_COMMIT, KEYQ, PUTCHAR, TICKS_MS

from .asm import addi, lw, sw
from .conftest import load


def _load_word(cpu, offset: int) -> int:
    mmio_base = 0x1000_0000
    cpu.write_reg(1, mmio_base)
    load(cpu, [lw(2, 1, offset)])
    cpu.step()
    return cpu.read_reg(2)


def _store_word(cpu, offset: int, value: int) -> None:
    mmio_base = 0x1000_0000
    cpu.write_reg(1, mmio_base)
    cpu.write_reg(2, value)
    load(cpu, [sw(1, 2, offset)])
    cpu.step()


def test_ticks_ms_derives_from_retired_instructions_not_wallclock(cpu_factory):
    cpu = cpu_factory(ipms=10)
    # Retire 25 instructions unrelated to TICKS_MS, then read it.
    load(cpu, [addi(3, 0, 1)] * 25)
    for _ in range(25):
        cpu.step()
    assert _load_word(cpu, TICKS_MS) == 25 // 10


def test_ticks_ms_is_elastic_same_result_any_speed(cpu_factory):
    # "Speed" here just means: however many *other* steps happen in
    # between, TICKS_MS only tracks icount, so two machines that retire the
    # same instructions see the same TICKS_MS regardless of how many wall
    # clock seconds or Python function calls that took.
    fast = cpu_factory(ipms=DEFAULT_IPMS)
    slow = cpu_factory(ipms=DEFAULT_IPMS)
    load(fast, [addi(3, 0, 1)] * 100)
    load(slow, [addi(3, 0, 1)] * 100)
    for _ in range(100):
        fast.step()
    for _ in range(100):
        slow.step()  # same work, could take arbitrarily different wall time
    assert _load_word(fast, TICKS_MS) == _load_word(slow, TICKS_MS)


def test_keyq_pops_fifo_in_push_order(cpu_factory):
    cpu = cpu_factory()
    cpu.memory.mmio.push_key(True, 0x1D)  # e.g. DOOM's "forward" key, pressed
    cpu.memory.mmio.push_key(False, 0x1D)  # same key, released
    assert _load_word(cpu, KEYQ) == (1 << 8) | 0x1D
    assert _load_word(cpu, KEYQ) == (0 << 8) | 0x1D


def test_keyq_empty_returns_zero_and_pops_nothing(cpu_factory):
    cpu = cpu_factory()
    assert _load_word(cpu, KEYQ) == 0
    assert _load_word(cpu, KEYQ) == 0  # still empty, not popping a phantom event
    cpu.memory.mmio.push_key(True, 0x20)
    assert _load_word(cpu, KEYQ) == (1 << 8) | 0x20
    assert _load_word(cpu, KEYQ) == 0


def test_exit_halts_with_reason_and_code(cpu_factory):
    cpu = cpu_factory()
    cpu.write_reg(1, 0x1000_0000)
    cpu.write_reg(2, 7)
    load(cpu, [sw(1, 2, EXIT)])
    try:
        cpu.step()
        raised = None
    except Halted as h:
        raised = h
    assert raised is not None
    assert raised.reason == HaltReason.EXIT
    assert raised.exit_code == 7


def test_putchar_appends_low_byte(cpu_factory):
    cpu = cpu_factory()
    _store_word(cpu, PUTCHAR, ord("D"))
    _store_word(cpu, PUTCHAR, 0x141)  # low byte 0x41 == 'A'; high bits dropped
    assert bytes(cpu.memory.mmio.console_out) == b"DA"


def test_frame_commit_records_frame_number_and_icount(cpu_factory):
    cpu = cpu_factory()
    load(cpu, [addi(3, 0, 1)] * 5)
    for _ in range(5):
        cpu.step()
    _store_word(cpu, FRAME_COMMIT, 42)
    assert cpu.memory.mmio.frame_commits[-1][0] == 42
    assert cpu.memory.mmio.frame_commits[-1][1] == cpu.icount - 1  # icount before the store itself


def test_no_host_clock_import(cpu_factory):
    # Static guard against the easiest way to reintroduce nondeterminism
    # (SPEC §8): grep the module source rather than trust review alone.
    import inspect

    from refemu import mmio as mmio_module

    src = inspect.getsource(mmio_module)
    for forbidden in ("time.time", "datetime.now", "import random", "perf_counter"):
        assert forbidden not in src
