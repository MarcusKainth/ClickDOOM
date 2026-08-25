"""MMIO register semantics (SPEC §3). Replaces `memory.NullMmio`'s plain
byte storage with the five registers' real behavior.

Word access only (SPEC §3's table header). DOOM's platform layer only ever
declares these as `volatile uint32_t *`, so a non-word access to one of the
five offsets below should never happen from compiled code; if it does, it
falls through to plain byte storage rather than triggering a side effect
partially -- SPEC §1 does not list a halt reason for it, so this file does
not invent one.

Elastic time (SPEC §3.1): `TICKS_MS` is `instructions_retired / IPMS`,
never wall clock. This module must never read any host clock or source of
randomness -- determinism (SPEC §8) depends on that being true not just
today but on every future edit here (see `test_no_host_clock_import`,
which greps this file's source for the usual ways that regresses).
"""

from __future__ import annotations

from collections import deque
from collections.abc import Callable

from .memory import MMIO_SIZE

DEFAULT_IPMS = 10_000  # instructions per emulated millisecond (SPEC §3.1)

# Register offsets within the MMIO region (SPEC §3).
TICKS_MS = 0x00
KEYQ = 0x04
EXIT = 0x08
PUTCHAR = 0x0C
FRAME_COMMIT = 0x10


class MmioExit(Exception):
    """Raised when the ROM writes `EXIT`. Not a SPEC §1 *fault* -- a clean,
    intentional stop with the written value as exit code. `cpu.py` turns
    this into a `Halted(HaltReason.EXIT, ...)` the same way it turns
    `BadAddr`/`Misaligned`/`SelfModify` into their halt reasons, so callers
    have one mechanism for "the machine stopped" regardless of cause.
    """

    def __init__(self, code: int):
        self.code = code
        super().__init__(f"EXIT with code {code}")


class Mmio:
    """SPEC §3 register semantics. `icount_fn` is a zero-arg callable
    returning the CPU's current retired-instruction count -- supplied by
    whoever wires this up (see `cpu.new_cpu`), because this class must not
    hold a reference to the CPU itself (that would be a cycle: CPU owns
    Memory owns Mmio).
    """

    def __init__(self, ipms: int = DEFAULT_IPMS, icount_fn: Callable[[], int] | None = None):
        self.ipms = ipms
        self.icount_fn = icount_fn
        self._backing = bytearray(MMIO_SIZE)  # fallback for width != 4
        self.key_queue: deque[tuple[int, int]] = deque()  # (pressed, doomkey), FIFO
        self.console_out = bytearray()
        self.frame_commits: list[tuple[int, int]] = []  # (frame_no, committed_icount)

    def push_key(self, pressed: bool, doomkey: int) -> None:
        """Ferry one key event in, as the driver does per PURITY.md (INSERT
        into `input_queue`). Order of calls is the pop order (event_seq)."""
        self.key_queue.append((1 if pressed else 0, doomkey & 0xFF))

    def read(self, offset: int, width: int) -> int:
        if width == 4 and offset == TICKS_MS:
            if self.icount_fn is None:
                raise RuntimeError("Mmio.icount_fn not wired up (use cpu.new_cpu)")
            return (self.icount_fn() // self.ipms) & 0xFFFF_FFFF
        if width == 4 and offset == KEYQ:
            if self.key_queue:
                pressed, doomkey = self.key_queue.popleft()
                return ((pressed << 8) | doomkey) & 0xFFFF_FFFF
            return 0
        return int.from_bytes(self._backing[offset : offset + width], "little")

    def write(self, offset: int, width: int, value: int) -> None:
        if width == 4 and offset == EXIT:
            raise MmioExit(value)
        if width == 4 and offset == PUTCHAR:
            self.console_out.append(value & 0xFF)
            return
        if width == 4 and offset == FRAME_COMMIT:
            if self.icount_fn is None:
                raise RuntimeError("Mmio.icount_fn not wired up (use cpu.new_cpu)")
            self.frame_commits.append((value, self.icount_fn()))
            return
        self._backing[offset : offset + width] = value.to_bytes(width, "little")
