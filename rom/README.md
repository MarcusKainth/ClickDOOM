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

`src/main_stub.c` was a placeholder giving `crt0`'s `call main` a real
symbol until libc and the real `DG_*` hooks landed — retired in #7, which
supersedes it with the real `src/main.c`. Verified at the time via
`readelf`/`objdump`/`nm` inside the toolchain container: entry point
`0x8000_0000`, `.text` contained exactly `_start` + `main` with no orphan
sections, `__bss_start`/`__bss_end` bound exactly the one `.bss` global
that existed, and the build was still byte-reproducible (same sha256
across a rebuild and after evicting the local toolchain image).

One thing this issue's "done when" couldn't close from `rom`'s side alone:
"reaches `main` in refemu without faulting." Confirmed since — `refemu`
rebuilt this ELF from the branch and booted it in their own interpreter:
10,000 instructions, no faults, landed exactly at `main`'s `for(;;)` loop
at `pc=0x80000048` after correctly executing the `bss_counter` increment.
First genuine cross-workstream integration on the project.

## Vendored sources and licensing (issue #41)

`rom/vendor/` holds unmodified upstream doomgeneric (which already carries
the full DOOM engine source) — see
[`rom/vendor/README.md`](vendor/README.md) for the pinned commit,
provenance, and integrity manifest. **The DOOM engine and doomgeneric are
GPL-2.0-or-later**; the license text is vendored verbatim at
[`rom/vendor/doomgeneric/LICENSE`](vendor/doomgeneric/LICENSE). This is
separate from the shareware `doom1.wad` licensing, which lands with the WAD
itself in issue #9.

Every deviation the port requires from that pristine tree — libc shim call
sites, the `DG_*` MMIO wiring, anything RV32IM-bare-metal-specific — is a
patch file in [`rom/patches/`](patches/README.md), never a hand-edit to
`rom/vendor/`.

## libc shims (issue #7)

The full DOOM engine (`toolchain/link.ld`'s `DOOM_SRCS` in `rom/Makefile`
— every vendored `.c` file except the other platforms' `doomgeneric_*.c`
ports, their audio backends, and a handful of files upstream's own default
Makefile also omits without `FEATURE_SOUND`) now links, against the
toolchain's bundled newlib rather than a hand-rolled libc: `-nostdlib` for
crt/startfiles (ours — issue #6), `-lc -lm -lgcc` for everything else
(malloc, string.h, printf, ...). `src/syscalls.c` is the actual "shim" —
the small, standard syscall surface newlib calls down into
(`_sbrk`/`_read`/`_write`/`_open`/`_close`/`_lseek`/`_fstat`/`_isatty`/
`_exit`/`_kill`/`_getpid`/`_link`/`_unlink`, plus `mkdir`, which newlib
doesn't stub at all), discovered empirically by linking the real engine
and reading the linker's undefined-symbol list rather than guessing at
DOOM's libc surface up front:

- **Heap (SPEC §2):** `_sbrk` bump-allocates between `link.ld`'s
  `__heap_start`/`__heap_end`, rejecting growth past either bound.
- **Console (SPEC §3 `PUTCHAR`):** `_write` on fd 1/2 stores each byte to
  the MMIO register directly — there's no batched-write register, so this
  is genuinely one store per byte, not an optimization left undone.
- **File I/O:** a tiny read-only virtual filesystem
  ([`src/rom_vfs.h`](src/rom_vfs.h)) backs `_open`/`_read`/`_lseek`/
  `_close`/`_fstat`. Empty until issue #9 calls `rom_vfs_register()` with
  the embedded WAD's bytes — an empty registry is a valid state, every
  `open()` just reports `ENOENT`. Writes (config/save-game files DOOM
  tries to create) get a "sink" fd instead of a hard failure: they report
  success and discard the bytes, since there's no writable storage for
  them to land in and "the write call failed" would risk DOOM treating a
  missing disk as fatal at startup.
- **Clean stop:** `_exit` (and `_kill`, which newlib's `abort()` routes
  through) writes SPEC §3's `EXIT` register — the only correct way to stop
  a machine with no OS to return to and no `ecall`/`ebreak` allowed (SPEC
  §1).

Verified by actually linking the real engine, not just compiling pieces of
it: zero undefined symbols, and — checked directly with `objdump` over the
whole ~600 KB `.text`, not asserted — **zero `ecall`/`ebreak`/CSR
instructions anywhere in the final binary**, including inside newlib's own
code. `toolchain/link.ld` also gained a `/DISCARD/` line for
`.init_array`/`.fini_array`: linking real `-lgcc` pulls in `crtbegin.o`'s
`register_fini` (C++-style atexit bookkeeping, unconditional even for a
pure C link), which `crt0` correctly never walks — discarding it is the
honest version of that decision rather than leaving unprocessed bytes
sitting in `.data`.

`src/dg_hooks_stub.c` was the "temporary, issue-#8-will-replace-this" file
— every `DG_*` hook a no-op standing in for the real SPEC §3 MMIO wiring.
`src/main.c` is not a placeholder: `doomgeneric_Create(0, 0)` is genuinely
what this ROM's `main` does (doomgeneric's own documented usage pattern),
since `D_DoomMain()` never returns.

## DG_* platform hooks (issue #8)

`src/dg_hooks.c` retires `src/dg_hooks_stub.c` with the real thing, wired
against SPEC §3 MMIO instead of SDL/X11/Win32. No patch to `rom/vendor/`
was needed for any of it:

- **8bpp palette-indexed framebuffer, not doomgeneric's default 32bpp
  RGBA (SPEC §2's whole rationale: 4x fewer stores per frame).**
  doomgeneric already ships exactly this mode — `-DCMAP256
  -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200` (`rom/Makefile`) makes
  `pixel_t` a `uint8_t` and sizes `DG_ScreenBuffer` to match
  `SCREENWIDTH`/`SCREENHEIGHT` (320x200, hardcoded in `i_video.h`,
  independent of the `DOOMGENERIC_RESX`/`RESY` knob) exactly, so
  doomgeneric's own internal scaling computes 1:1 and `DG_ScreenBuffer` is
  already byte-identical to SPEC §2's `FRAMEBUFFER` region before
  `DG_DrawFrame` even runs. `DG_DrawFrame` copies those 64,000 bytes as
  16,000 word stores (not byte stores — that 4x store-count win is the
  actual point of the 8bpp choice, so a byte-at-a-time copy would throw
  half of it away), then writes `FRAME_COMMIT`.
- **Palette:** doomgeneric's `CMAP256` mode exposes `colors[256]` /
  `palette_changed` as `extern` globals from `i_video.c` specifically for
  a platform to consume this way — there's no `DG_SetPalette` hook in
  doomgeneric.h because none is needed. `DG_DrawFrame` packs them into 768
  bytes (192 word stores) and writes SPEC §2's `PALETTE` region only when
  `palette_changed`, clearing the flag after. `colors[]` is
  gamma-corrected (`I_SetPalette` applies `gammatable[usegamma]`); SPEC
  doesn't say anything about gamma, and reading the already-corrected
  palette is what every other doomgeneric `CMAP256` port does.
- **`DG_GetTicksMs`/`DG_GetKey`:** direct reads of `TICKS_MS`/`KEYQ`.
  `DG_GetKey` returns 0 on an empty queue (matching `i_input.c`'s `while
  (DG_GetKey(&pressed, &key))` drain-loop contract) and decodes SPEC
  §3.2's `(pressed << 8) | doomkey` otherwise.
- **`DG_SleepMs`:** a bounded busy-poll on `TICKS_MS` until it advances by
  the requested amount — not a real sleep, deliberately. SPEC §3.1's
  elastic time means "waiting" *is* retiring instructions; a real
  wall-clock wait would be exactly the kind of host-environment read SPEC
  §8 forbids on a computation path.
- **`EXIT` on program stop** was already covered by issue #7's
  `_exit`/`_kill` shims (newlib's `exit()`/`abort()` both route through
  them) — nothing new needed here.

**Verified by actually booting, not just building.** Since #9 (the WAD)
hasn't landed, the real DOOM engine can't reach `DG_DrawFrame`/`DG_GetKey`
yet — it fails at IWAD search first (`I_Error`, confirmed via a refemu
boot: halts `EXIT` at icount≈827K, matching the finding from issue #7's
review). So issue #8's "each hook exercised at least once in a refemu
boot" was proven with a small scratch harness (not committed — crt0 +
syscalls + `dg_hooks.c` + a `main` that calls each `DG_*` function
directly with known test data, in place of the full engine) booted
through refemu with a key event pre-pushed via `Mmio.push_key()`:

```
result_ticks0 = 4                          # DG_GetTicksMs: reads straight through
result_had_event_1 = 1, pressed=1, key=0x1d  # DG_GetKey: pre-pushed event, correctly decoded
result_had_event_2 = 0                     # DG_GetKey: empty queue after the pop
result_ticks_after_sleep = 54              # DG_SleepMs(50): 54 - 4 == 50, exact
frame_commits = [(0, 883091), (1, 963115)] # DG_DrawFrame x2: FRAME_COMMIT = 0, then 1
framebuffer matches expected pattern: True # byte-exact, all 64,000 bytes
palette matches expected pattern: True     # byte-exact, all 768 bytes
```

Every hook, exercised, MMIO trace matching SPEC §3 exactly — not asserted,
read back from the emulator's own memory after the run.
