# rom/

See CLAUDE.md for this workstream's charter and SPEC.md for its contracts.
Ownership is claimed via issue self-assignment.

## Build

    just build-rom

produces `rom/build/doom-rv32im.bin` (and `.elf`). Nothing beyond Docker is
required on the host — `rom/Makefile` builds a toolchain container
(`rom/toolchain/Dockerfile`) and runs every compile/link step inside it.

**What's actually guaranteed reproducible is the ROM artifact, not the
Docker image.** `doom-rv32im.bin`'s sha256 is stable across repeat builds
and after evicting the local image cache (verified) because it depends
only on the toolchain binaries that do the real work — `riscv-none-elf-gcc`
et al., fetched as a specific uploaded release asset and checked against a
sha256 pinned independently of the upload (see "Toolchain" below) — not on
which `curl`/`ca-certificates`/`xz-utils` happened to fetch and unpack
them. Those apt packages are intentionally left unpinned to a specific
version: they float with Debian's `bookworm-slim` security updates, so the
image's own layer digests are *not* guaranteed byte-stable over time, and
that's fine — nothing in that layer reaches the output. Don't read a
different image digest on a later build as evidence of a compromised or
nondeterministic build; check `doom-rv32im.bin`'s sha256 (or
`rom/PINNED_HASH`, once #10 lands), which is the thing SPEC §4 actually
promises.

## Toolchain (issue #5)

**xPack GNU RISC-V Embedded GCC v15.2.0-1** (`riscv-none-elf-*`), a
bare-metal ("no known OS") target — matches the charter, since rom/ brings
its own crt0 (#6) and libc shims (#7) rather than linking newlib against an
OS that doesn't exist here. Fetched in the Dockerfile from the upstream
GitHub release and verified against a sha256 pinned from the Releases API
asset digest (independent of xpack's own `.sha` file). Base image
(`debian:bookworm-slim`) is pinned by manifest-list digest, not a floating
tag.

Both are content-pinned, so a bump is a deliberate `ci:`/`rom:` PR that
changes the pin, never something that happens by re-pulling `latest`.

## crt0 and memory map (issue #6)

`toolchain/link.ld` implements SPEC §2's memory map inside the 24 MiB RAM
window at `0x8000_0000`: `.text` (crt0 + all compiled code) first and its
own contiguous, word-aligned region so `[__text_start, __text_end)` is
exactly what CI later pins as `text_start`/`text_end` in `manifest.json`
(#10) and what the executor pre-decodes (ADR-0002) — a store anywhere in
there is `SELF_MODIFY`. `.rodata`, `.data`, then `.bss` (`NOLOAD`, so it
costs no file bytes and `objcopy -O binary` correctly stops emitting bytes
at end of `.data`) follow. The heap is everything between end-of-bss and a
1 MiB stack reserved at the top of RAM (`STACK_SIZE` in the linker script —
a rom/-local choice, not SPEC-mandated). Three `PHDRS` (R+E / R / R+W) keep
the ELF's segment permissions honest; the SQL CPU has no MMU so this has no
effect on the flat binary, it's purely so `readelf -l` doesn't lie.

`src/crt0.S` does exactly the three things SPEC §1 asks of it: set `sp`,
zero `.bss`, call `main`. No `gp` setup — `rom/Makefile` passes `-mno-relax`
so gp-relative addressing is never emitted, and `-msmall-data-limit=0` so
gcc never routes small globals into `.sdata`/`.sbss` in the first place
(found by inspecting a build: with only `-mno-relax`, a small `.bss` global
still landed in an orphan `.sbss` section outside `[__text_start,
__text_end)`'s zero range and was silently never zeroed — `link.ld`'s
`.bss`/`.data`/`.rodata` output sections now also fold in the `.s*` names
directly, as defense in depth against the same class of bug from an object
built with different flags).

`src/main_stub.c` is **not** doomgeneric's `main` — it's a placeholder that
gives `crt0`'s `call main` a real symbol until #7-#9 land libc and the real
`DG_*` hooks. Verified via `readelf`/`objdump`/`nm` inside the toolchain
container: entry point `0x8000_0000`, `.text` contains exactly `_start` +
`main` with no orphan sections, `__bss_start`/`__bss_end` bound exactly the
one `.bss` global that exists, and the build is still byte-reproducible
(same sha256 across a rebuild and after evicting the local toolchain image).

One thing this issue's "done when" can't fully close yet: "reaches `main`
in refemu without faulting" needs refemu's RV32I interpreter core (#11),
which hasn't landed. Verified everything checkable without it (ELF
structure, disassembly, reproducibility); asked refemu to confirm the boot
once #11 lands.
