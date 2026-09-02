# Licensing

Four sets of terms apply in this repository. The boundary follows the
directory tree, so the file you are looking at tells you which set you are
under.

| Path | Terms |
|---|---|
| `rom/` | GPL-2.0-or-later ([`rom/LICENSE`](rom/LICENSE)) |
| `rom/wad/doom1.wad` | id Software shareware distribution terms |
| `native/` | GPL-3.0-or-later ([`native/LICENSE`](native/LICENSE)) |
| Everything else | Apache-2.0 ([`LICENSE`](LICENSE)) |

## Apache-2.0 covers the emulator and the tooling

`spec/`, `refemu/`, `sqlcpu/`, `executor/`, `driver/`, `scripts/` and `docs/`
are an RV32IM machine emulator, the driver that runs both modes, and the
tooling around them. They implement an instruction set and a client, not a
game. None of them contains DOOM source, and none of them is compiled into the
ROM image. The emulator runs the ROM the way any emulator runs a program it did
not write.

Two files describe the engine without reproducing it. `spec/src/native_state.rs`
names the fields native mode carries between tics, with their types, which is
the interface both sides of a differential write. `refemu/probe/layout.tsv`
holds the offsets of those fields in the ROM's structs, computed by compiling
the engine's own headers. Both are facts about the program rather than its
expression, and they stay under Apache-2.0.

## GPL-2.0-or-later covers `rom/`

`rom/` builds the DOOM binary, so everything in it is part of one program with
the upstream engine.

- `rom/vendor/doomgeneric/` is [ozkl/doomgeneric](https://github.com/ozkl/doomgeneric)
  at commit `dcb7a8dbc7a16ce3dda29382ac9aae9d77d21284`, unmodified. It carries
  id Software's DOOM engine sources. Its files state "either version 2 of the
  License, or (at your option) any later version", which is where the
  `-or-later` comes from.
- `rom/patches/` patches those sources at build time.
- `rom/src/` (crt0, the linker script, libc shims, `dg_hooks.c`, `syscalls.c`)
  is compiled and linked with them.

`rom/vendor/README.md` records the provenance and the integrity manifest.
`rom/patches/README.md` records every deviation from upstream.

## GPL-3.0-or-later covers `native/`

`native/` is a derivative work of the engine, in two ways.

- `native/tables/` holds the engine's own data, copied out of `info.c`,
  `tables.c`, `m_random.c`, `r_draw.c`, `p_spec.c` and `p_switch.c` under
  `rom/vendor/` by `native/src/bin/gen_tables.rs`: the state and mobj tables,
  the sprite names, the weapon table, the sine, tangent and arctangent tables,
  the random table, the gamma table and the rest.
- `native/src/sql/` and `native/sql/` reproduce the engine's simulation and
  renderer function by function as ClickHouse SQL, quirks included, from
  `p_map.c`, `p_doors.c`, `p_plats.c`, `p_floor.c`, `r_segs.c`, `r_plane.c`
  and their neighbours. Each module names the C file it reproduces.

The upstream terms allow any later version of the GPL, and this directory takes
that option: it is distributed under version 3 or later. Its terms are in
[`native/LICENSE`](native/LICENSE).

## The `clickdoom` binary

`driver/` links `native/` into the `clickdoom` binary, so a built binary is one
combined program of Apache-2.0 and GPL-3.0-or-later code. Apache-2.0 is
compatible with version 3 of the GPL, and a built `clickdoom` is distributed
under the GPL-3.0's terms as a whole. The driver's own sources stay under
Apache-2.0; what changes is the terms a binary carries.

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
GPL-2.0-or-later inside `rom/`, GPL-3.0-or-later inside `native/`, Apache-2.0
§5 everywhere else. Inbound is the same as outbound. There is no CLA and
nothing to sign.
