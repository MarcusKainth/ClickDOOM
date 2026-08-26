# rom/wad/ — the embedded shareware WAD

`doom1.wad` is **DOOM Shareware v1.9** — the same release most source ports
and WAD archives treat as canonical, and the one id Software itself
distributed as freely redistributable marketing material for the retail
game. It contains one episode (nine maps: "Knee-Deep in the Dead") and
none of the retail/commercial episodes.

## Licensing position (this is the load-bearing claim; read it before
touching this file)

This file is **not** covered by DOOM's GPL source license -- it is game
*data*, not source code. It is embedded here under id Software's own
shareware distribution terms: the shareware episode has always been
explicitly freely redistributable, which is why it ships in, among many
other places, Debian's own package archive (`doom-wad-shareware`, itself
this exact v1.9 release, patched-and-repackaged as `1.9.fixed-5`).

**Do not replace this file with a commercial/retail IWAD** (`doom.wad`,
`doom2.wad`, `plutonia.wad`, `tnt.wad`, ...) or any other WAD whose
distribution terms haven't been checked. Per the root `README.md`: "Do not
embed commercial WADs in the repo." This file existing here is a
deliberate exception, not a precedent for embedding others.

## Provenance

Sourced from [`Doom-Utils/shareware-collection`](https://github.com/Doom-Utils/shareware-collection),
a GitHub organization that collects historical shareware releases
specifically for redistribution (mirroring the same reasoning `rom/vendor/`
uses for doomgeneric: vendor in-tree from a fixed, inspectable source
rather than fetch at build time from a host with no continuity guarantee)
-- `Doom 1.9/doom1.wad`, fetched directly, not re-derived or repacked.

Independently corroborated against two more sources before vendoring, not
just the one download:

| | |
|---|---|
| Size | 4,196,020 bytes |
| SHA-256 | `1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771` |
| MD5 | `f0cefca49926d00903cf57551d901abe` |
| SHA-1 | `5b2e249b9c5133ec987b3ea77596381dc0d6bc1d` |

All three checksums were computed locally from the downloaded bytes (not
copied from a webpage) and cross-checked against
[wad-archive.com's independent catalog entry](https://www.wad-archive.com/wad/5b2e249b9c5133ec987b3ea77596381dc0d6bc1d)
for "Doom Shareware v1.9" -- exact match on all three. Debian's
`doom-wad-shareware` source package (patch series `1.9.fixed-5`) is a
fourth independent confirmation that this specific release is the one
long-established as freely redistributable.

Verify the vendored file hasn't drifted:

```sh
cd rom/wad && shasum -a 256 -c doom1.wad.sha256sum
```

## Size, against SPEC §2's 24 MiB RAM budget

The WAD (4,196,020 B ≈ 4.00 MiB) plus the current engine binary
(593,136 B ≈ 0.57 MiB, per issue #7's `doom-rv32im.bin`) totals
**≈ 4.57 MiB**, leaving **≈ 19.43 MiB** of SPEC §2's 24 MiB RAM window for
heap, stack, `.bss`, and everything else the running engine allocates.
Healthy headroom -- worth having on record now rather than discovering a
tight fit during a boot that mysteriously fails to allocate.

## Not wired up yet

This directory is deliberately just the vendored file and its provenance
-- no embedding mechanism (`.incbin`, an object file, a `rom_vfs_register()`
call) lands here. That's issue #9's wiring half, coordinated separately
with the `rom` workstream since it touches files they're actively editing
(`rom/Makefile`'s source list in particular). Landing the vendored file on
its own first mirrors how issue #41/#44 vendored doomgeneric before #7
wired it into the build.
