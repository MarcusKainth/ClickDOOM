import pytest

from refemu.cpu import CPU, new_cpu
from refemu.memory import RAM_BASE, Memory


@pytest.fixture
def cpu():
    return CPU(memory=Memory())


@pytest.fixture
def cpu_factory():
    """A CPU with real MMIO semantics wired up (see `cpu.new_cpu`), for
    tests that need TICKS_MS/KEYQ/EXIT/PUTCHAR/FRAME_COMMIT behavior rather
    than just memory-map bounds checking. A factory, not a plain fixture,
    since some tests (elastic-time comparisons) need more than one CPU."""
    return new_cpu


def load(cpu: CPU, words: list[int], base: int = RAM_BASE) -> None:
    """Inject `words` into RAM at `base` and point pc at the first one.

    Uses `load_image` (SPEC §4's "loaded verbatim") rather than
    `memory.write`, so this works even when `base` falls inside a test's
    declared text region -- ROM loading is not a CPU store and is not
    subject to the SELF_MODIFY check.
    """
    data = b"".join(w.to_bytes(4, "little") for w in words)
    cpu.memory.load_image(data, base=base)
    cpu.pc = base
