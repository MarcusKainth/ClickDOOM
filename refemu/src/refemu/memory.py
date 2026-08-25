"""Flat physical memory for the RV32IM emulator (SPEC §2).

Four fixed regions, byte-addressable, little-endian. Anything outside them
is a fatal `BAD_ADDR` halt (SPEC §1). Within RAM, the `[text_start,
text_end)` sub-range is the immutable text region (ADR-0002): a store there
raises `SelfModify` instead of landing.

MMIO register *semantics* (TICKS_MS derivation, KEYQ pop, EXIT, ...) are not
this module's job — see SPEC §3 and issue #13. This module only enforces
the memory *map*: which addresses exist, and plain byte storage for the
region that doesn't have a device model plugged in yet. `Memory` accepts an
optional `mmio` handler (see `NullMmio` below); issue #13 replaces it with
one that has real register semantics.
"""

from __future__ import annotations

RAM_BASE = 0x8000_0000
RAM_SIZE = 24 * 1024 * 1024

MMIO_BASE = 0x1000_0000
MMIO_SIZE = 4 * 1024

FRAMEBUFFER_BASE = 0x1100_0000
FRAMEBUFFER_SIZE = 64_000

PALETTE_BASE = 0x1101_0000
PALETTE_SIZE = 768


class BadAddr(Exception):
    """Any access outside the four SPEC §2 regions."""

    def __init__(self, addr: int):
        self.addr = addr
        super().__init__(f"address 0x{addr:08x} outside declared regions")


class Misaligned(Exception):
    """A half/word access not aligned to its own width (SPEC §1)."""

    def __init__(self, addr: int, width: int):
        self.addr = addr
        self.width = width
        super().__init__(f"misaligned {width}-byte access at 0x{addr:08x}")


class SelfModify(Exception):
    """A store landing inside the immutable text region (SPEC §1, ADR-0002)."""

    def __init__(self, addr: int):
        self.addr = addr
        super().__init__(f"store into text region at 0x{addr:08x}")


class NullMmio:
    """Plain byte storage for the MMIO region: no device semantics.

    Placeholder until issue #13 wires up TICKS_MS/KEYQ/EXIT/PUTCHAR/
    FRAME_COMMIT. Behaves like ordinary RAM so the RV32I core (issue #11)
    can exercise loads and stores that happen to land in the MMIO region
    without needing a device model yet.
    """

    def __init__(self):
        self._backing = bytearray(MMIO_SIZE)

    def read(self, offset: int, width: int) -> int:
        return int.from_bytes(self._backing[offset : offset + width], "little")

    def write(self, offset: int, width: int, value: int) -> None:
        self._backing[offset : offset + width] = value.to_bytes(width, "little")


class Memory:
    """SPEC §2's four regions, plus the text-region write protection."""

    def __init__(
        self,
        ram_size: int = RAM_SIZE,
        mmio: NullMmio | None = None,
        text_start: int | None = None,
        text_end: int | None = None,
    ):
        self.ram = bytearray(ram_size)
        self.ram_size = ram_size
        self.mmio = mmio if mmio is not None else NullMmio()
        self.framebuffer = bytearray(FRAMEBUFFER_SIZE)
        self.palette = bytearray(PALETTE_SIZE)
        # [text_start, text_end): read-only text region (ADR-0002). Either
        # None disables the check — riscv-tests have no ROM manifest to
        # source these bounds from.
        self.text_start = text_start
        self.text_end = text_end

    def set_text_region(self, text_start: int, text_end: int) -> None:
        self.text_start = text_start
        self.text_end = text_end

    def _in_text(self, addr: int, width: int) -> bool:
        if self.text_start is None or self.text_end is None:
            return False
        return not (addr + width <= self.text_start or addr >= self.text_end)

    @staticmethod
    def _check_align(addr: int, width: int) -> None:
        if width > 1 and addr % width != 0:
            raise Misaligned(addr, width)

    def read(self, addr: int, width: int) -> int:
        """Read `width` bytes (1, 2 or 4) little-endian at `addr`."""
        self._check_align(addr, width)
        if RAM_BASE <= addr and addr + width <= RAM_BASE + self.ram_size:
            off = addr - RAM_BASE
            return int.from_bytes(self.ram[off : off + width], "little")
        if MMIO_BASE <= addr and addr + width <= MMIO_BASE + MMIO_SIZE:
            return self.mmio.read(addr - MMIO_BASE, width)
        if FRAMEBUFFER_BASE <= addr and addr + width <= FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE:
            off = addr - FRAMEBUFFER_BASE
            return int.from_bytes(self.framebuffer[off : off + width], "little")
        if PALETTE_BASE <= addr and addr + width <= PALETTE_BASE + PALETTE_SIZE:
            off = addr - PALETTE_BASE
            return int.from_bytes(self.palette[off : off + width], "little")
        raise BadAddr(addr)

    def write(self, addr: int, width: int, value: int) -> None:
        """Write the low `width` bytes of `value` little-endian at `addr`."""
        self._check_align(addr, width)
        value &= (1 << (width * 8)) - 1
        if RAM_BASE <= addr and addr + width <= RAM_BASE + self.ram_size:
            if self._in_text(addr, width):
                raise SelfModify(addr)
            off = addr - RAM_BASE
            self.ram[off : off + width] = value.to_bytes(width, "little")
            return
        if MMIO_BASE <= addr and addr + width <= MMIO_BASE + MMIO_SIZE:
            self.mmio.write(addr - MMIO_BASE, width, value)
            return
        if FRAMEBUFFER_BASE <= addr and addr + width <= FRAMEBUFFER_BASE + FRAMEBUFFER_SIZE:
            off = addr - FRAMEBUFFER_BASE
            self.framebuffer[off : off + width] = value.to_bytes(width, "little")
            return
        if PALETTE_BASE <= addr and addr + width <= PALETTE_BASE + PALETTE_SIZE:
            off = addr - PALETTE_BASE
            self.palette[off : off + width] = value.to_bytes(width, "little")
            return
        raise BadAddr(addr)

    def load_image(self, data: bytes, base: int = RAM_BASE) -> None:
        """Load a flat ROM image into RAM at `base` (SPEC §4)."""
        off = base - RAM_BASE
        self.ram[off : off + len(data)] = data
