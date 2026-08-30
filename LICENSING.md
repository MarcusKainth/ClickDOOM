# Licensing

Three sets of terms apply in this repository. The boundary follows the
directory tree, so the file you are looking at tells you which set you are
under.

| Path | Terms |
|---|---|
| `rom/` | GPL-2.0-or-later ([`rom/LICENSE`](rom/LICENSE)) |
| `rom/wad/doom1.wad` | id Software shareware distribution terms |
| Everything else | Apache-2.0 ([`LICENSE`](LICENSE)) |

## Apache-2.0 covers the emulator

`refemu/`, `sqlcpu/`, `executor/`, `driver/`, `scripts/` and `docs/` are an
RV32IM machine emulator and the tooling around it. They implement an
instruction set, not a game. None of them contains or derives
from DOOM source, and none of them is compiled into the ROM image. They run the
ROM the way any emulator runs a program it did not write.

## GPL-2.0-or-later covers `rom/`

`rom/` builds the DOOM binary, so everything in it is part of one program with
the upstream engine.

- `rom/vendor/doomgeneric/` is [ozkl/doomgeneric](https://github.com/ozkl/doomgeneric)
  at commit `dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284`, unmodified. It carries
  id Software's DOOM engine sources. 176 of its files state "either version 2 of
  the License, or (at your option) any later version", which is where the
  `-or-later` comes from.
- `rom/patches/` patches those sources at build time.
- `rom/src/` (crt0, the linker script, libc shims, `dg_hooks.c`, `syscalls.c`)
  is compiled and linked with them.

`rom/vendor/README.md` records the provenance and the integrity manifest.
`rom/patches/README.md` records every deviation from upstream.

## The WAD is under neither

`rom/wad/doom1.wad` is DOOM Shareware v1.9, sha256
`1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771`. It is game
data, not source, so DOOM's GPL release does not cover it. It ships here under
id Software's own shareware distribution terms, which have always allowed
redistribution of the shareware episode.

Do not add a commercial IWAD (`doom.wad`, `doom2.wad`, `plutonia.wad`,
`tnt.wad`) or any WAD whose distribution terms have not been checked.
`rom/wad/README.md` has the full provenance and the checksums it was verified
against.

## Contributing

Contributions are accepted under the terms of the directory they land in:
Apache-2.0 §5 outside `rom/`, GPL-2.0-or-later inside it. Inbound is the same as
outbound. There is no CLA and nothing to sign.
