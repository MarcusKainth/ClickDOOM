"""Minimal ELF32 little-endian symbol-table reader.

Deliberately dependency-free (no pyelftools, no shelling out to the pinned
toolchain container): the E7 harness has to run from a bare `uv run` inside
refemu/, and adding a dependency to refemu's pyproject.toml for a bench
script would be a production change. Only what E7 needs: STT_FUNC symbols
with their value and size, so a retired pc can be mapped to a function name.
"""

from __future__ import annotations

import struct
from pathlib import Path

STT_FUNC = 2
SHT_SYMTAB = 2


def read_func_symbols(elf_path: str | Path) -> list[tuple[int, int, str]]:
    """Return sorted [(addr, size, name)] for every STT_FUNC symbol."""
    data = Path(elf_path).read_bytes()
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        raise ValueError("not a little-endian ELF32 file")

    (e_shoff,) = struct.unpack_from("<I", data, 0x20)
    (e_shentsize,) = struct.unpack_from("<H", data, 0x2E)
    (e_shnum,) = struct.unpack_from("<H", data, 0x30)

    sections = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        name, sh_type, flags, addr, sh_off, size, link, info, align, entsize = struct.unpack_from(
            "<10I", data, off
        )
        sections.append((sh_type, sh_off, size, link, entsize))

    syms: list[tuple[int, int, str]] = []
    for sh_type, sh_off, size, link, entsize in sections:
        if sh_type != SHT_SYMTAB:
            continue
        str_off, str_size = sections[link][1], sections[link][2]
        strtab = data[str_off : str_off + str_size]
        for j in range(size // entsize):
            o = sh_off + j * entsize
            st_name, st_value, st_size, st_info, st_other, st_shndx = struct.unpack_from(
                "<IIIBBH", data, o
            )
            if (st_info & 0xF) != STT_FUNC:
                continue
            end = strtab.index(b"\x00", st_name)
            nm = strtab[st_name:end].decode("utf-8", "replace")
            if nm:
                syms.append((st_value, st_size, nm))

    # Deterministic order regardless of symtab order: address, then size,
    # then name. Duplicate addresses (aliases) keep a stable winner.
    syms.sort(key=lambda s: (s[0], s[1], s[2]))
    return syms
