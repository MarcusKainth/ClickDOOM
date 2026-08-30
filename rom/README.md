# rom/

See CLAUDE.md for this workstream's charter and SPEC.md for its contracts.
Ownership is claimed via issue self-assignment.

## Build

    make build-rom

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
`src/main.c` is not a placeholder: `doomgeneric_Create(0, 0)` followed by
`doomgeneric_Tick()` in a loop is genuinely what this ROM's `main` does
(doomgeneric's own documented usage pattern). Issue #9's "corrected below"
section explains why that second half was missing until a real WAD made
the bug visible.

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

## Embedding the WAD (issue #9), and a real bug it uncovered

`src/wad_embed.S` embeds `rom/wad/doom1.wad` (vendored in #60) as a rodata
blob via `.incbin` — `_wad_doom1_start`/`_wad_doom1_end` bracket it.
`src/wad_embed.c`'s `wad_embed_register()` hands that range to
`syscalls.c`'s virtual filesystem (`rom_vfs_register`, the seam #7 built
for exactly this); `src/main.c` calls it before `doomgeneric_Create()`,
since `D_DoomMain`'s IWAD search happens synchronously inside that call.
Explicit call, not a constructor — see `wad_embed.h` for why (crt0 never
processes `.init_array`; verified empirically before this landed).

**Corrected a real bug in `src/main.c`, found by actually booting with a
real WAD for the first time.** The previous version called
`doomgeneric_Create()` alone, on the assumption (stated in its own
comment) that `D_DoomMain()` "ends in `D_DoomLoop()`, which never
returns." That's true of *vanilla* DOOM's `D_DoomLoop` — not of
doomgeneric's redesigned one (`d_main.c`): it runs one-time graphics setup
plus exactly **one** `doomgeneric_Tick()` and returns, on the (correct,
and documented) assumption that the platform's own loop calls
`doomgeneric_Tick()` again for every tic after that. Without a WAD,
`D_DoomMain` halts at the IWAD search (issue #7's finding) long before
ever reaching `D_DoomLoop`, so the missing loop was invisible until now.

Caught by booting the built ELF in refemu and finding `pc` parked at
`main`'s own safety `for(;;)` after exactly one `FRAME_COMMIT`, instead of
climbing past it:

```
frame_commits: [(0, 13243948)]      # one tic ran, then D_DoomLoop returned
# pc == main+0x20 (this file's own for(;;)) at both 30M and 80M instructions
# -- not "slow", genuinely stuck: identical pc at two very different icounts
```

Fixed by adding the missing loop (`for (;;) { doomgeneric_Tick(); }`).
Rebuilt and re-booted the same way:

```
$ uv run python3 -c '<boot rom/build/doom-rv32im.bin through refemu new_cpu(), run 60M instructions>'
did not halt within 60000000: icount=60000000 pc=0x80037aa4
total frame_commits: 100
first 10: [(0, 13243964), (1, 13715122), (2, 14185231), (3, 14656377), (4, 15127523), ...]
last 5:   [(95, 57969584), (96, 58439699), (97, 58910869), (98, 59382027), (99, 59852142)]
```

100 frames, steadily advancing `pc`, a stable ≈470K instructions per tic
after the (much heavier, one-time) first frame's graphics/game-state
setup. The console log along the way shows the shareware WAD loading, the
DOOM Shareware banner, and the full engine init sequence
(`R_Init`/`P_Init`/`S_Init`/`I_InitGraphics: ... 320 x 200, bpp: 8 ...
Auto-scaling factor: 1`) — confirming issue #8's `CMAP256` sizing
computed exactly the 1:1 scaling it was designed for. This is the first
time the real DOOM engine has run past its own startup sequence anywhere
in this project.

### ROM size, against SPEC §2's 24 MiB RAM window (post-wiring, real numbers)

Computed from the actual linked ELF's symbols, not estimated:

| region | bytes | |
|---|---:|---|
| `.text` | 395,196 | 385.9 KiB |
| `.rodata`/`.data` | 4,394,116 | 4.19 MiB |
| — of which the WAD | 4,196,020 | 4.00 MiB |
| — other rodata/data | 198,096 | 193.5 KiB |
| `.bss` | 245,384 | 239.6 KiB |
| heap (free) | 19,082,552 | **18.20 MiB** |
| stack (reserved) | 1,048,576 | 1.00 MiB |
| **total** | 25,165,824 | **24.00 MiB**, exact |

18.20 MiB of headroom for whatever the running engine allocates at
runtime (zone memory, level data, ...) — healthy, and slightly less than
#60's pre-wiring 19.43 MiB estimate (that estimate didn't account for
`.bss`, the 1 MiB stack reservation, or the ~193 KiB of non-WAD
rodata/data the full link adds).

### Does `-timedemo` pay `DG_SleepMs`'s elastic-time tax?

Raised during #59's review: `DG_SleepMs`'s busy-poll on `TICKS_MS` is the
only correct implementation (SPEC §3.1's elastic time means "waiting" is
retiring instructions, not blocking on a real clock — see the "DG_*
platform hooks" section above), but under that model a sleep is never
free: waiting N ms costs N × `IPMS` instructions (10,000/ms by default)
doing no game work. If interactive play sleeps ~28ms between tics at 35
fps, that's real overhead — the question was whether Phase 3's
`-timedemo demo3` run pays it too.

Read `d_loop.c`/`g_game.c` rather than assuming. It doesn't, and not by
luck:

- `G_TimeDemo` (`g_game.c`, what `-timedemo` calls) sets `singletics =
  true`.
- `d_loop.c`'s only two `I_Sleep`/`DG_SleepMs` call sites: one
  (`BlockUntilStart`, networking-only) is wrapped in `#if ORIGCODE`, and
  `config.h` permanently `#undef`s `ORIGCODE` — that call site isn't even
  compiled into this ROM. The other is `TryRunTics`'s wait-for-more-tics
  loop, reached only if `lowtic < gametic/ticdup + counts` — but in
  singletics mode, `BuildNewTic()` (called unconditionally, first thing in
  `TryRunTics`) synchronously advances `maketic` by exactly one tic before
  that condition is even checked, and `counts` is derived from the same
  freshly-advanced `lowtic`. The wait condition can't be true by
  construction: singletics mode is specifically designed to never wait,
  which is the entire point of a throughput-measuring timedemo.

So: `DG_SleepMs` costs nothing on the path that is this project's
Definition of Victory. It only matters for interactive play (the stretch
goal), where pacing at 35 tics/sec is the actual intent, not a cost to
avoid. Doesn't change `IPMS` (still SPEC §9's deliberately-unratified
open question, owned by `refemu` per SPEC §9), but rules out one way the
elastic-time model could have quietly taxed the multi-week timedemo run.

### Palette gamma (a note for whoever writes the render query, #29)

`colors[]` (the `CMAP256` extern this file reads for the `PALETTE`
region) is gamma-corrected — `I_SetPalette` applies
`gammatable[usegamma]` to the raw WAD palette before storing it there.
What lands in SPEC §2's `PALETTE` region is already post-gamma. This
can't cause a `refemu`/`sqlcpu` divergence (both engines just store
whatever bytes the ROM writes, gamma-applied or not), but the render
query turning `PALETTE` bytes into displayable RGB must not apply gamma a
second time.

## manifest.json and PINNED_HASH (issue #10)

Closes the ROM contract SPEC §4 defines: `manifest.json` and
`rom/PINNED_HASH`, both emitted by the build, not hand-written.

`toolchain/gen_manifest.sh` runs inside the pinned container immediately
after the ELF/flat-binary build and reads every field from the actual
build artifacts:

- `entry`, `load_addr` — from `readelf -h`/`readelf -l`'s real entry point
  and first `LOAD` segment address (both `0x8000_0000` = `2147483648` by
  design, but read from the ELF rather than assumed).
- `text_start`, `text_end` — from `nm`'s `__text_start`/`__text_end`
  symbols, the same `toolchain/link.ld` symbols that delimit the
  `SELF_MODIFY`-protected region (SPEC §1/§2, ADR-0002). This is the field
  pair SPEC §4 specifically calls out as build-emitted rather than
  hand-transcribed — a hand-copied bound that drifted from the linker
  script would silently disable that protection, the worst failure mode
  for a check whose entire purpose is catching silent corruption.
- `size`, `sha256` — computed directly from the built `.bin`.
- `spec_version` — the one field that isn't derived from the build; it's a
  Makefile constant (`SPEC_VERSION`) kept in sync with `SPEC.md`'s own
  version by hand, per `SPEC.md`'s own instruction ("must update
  SPEC_VERSION here and the `spec_version` constants in code").

Verified end to end: `manifest.json` is byte-identical across a plain
rebuild and a full cache-evicted rebuild, same as `doom-rv32im.bin`
always has been.

`rom/PINNED_HASH` is the current build's sha256 (as of issue #10's
original landing, `22113f55234fa050dbd9ece64b8d713451b32ddc6b2b3f3c289c2bff87c955ed`;
it has since moved twice more, most recently to `9a6a47d0…` for #175's
unroll — check the file itself, never this paragraph, for the live
value). `make check-pinned-hash` mirrors the verification CI's own
(already present, human-gated `ci.yml`) hash-check step runs once this
file exists — tested both directions locally: passes against the real
build, and fails loudly with the exact "P0 or update the pin" message
when the committed hash doesn't match (simulated with a corrupted
`PINNED_HASH`, restored before committing). From this point on, **any
ROM-affecting change must update `rom/PINNED_HASH` in the same PR** —
CLAUDE.md's third non-negotiable is that a mismatch elsewhere is
information (a nondeterminism P0), never something to "fix" by editing
the pin.

### The dependency runs the other way too: reference traces are PINNED_HASH-keyed (issue #214)

The paragraph above states the forward rule — a ROM-affecting change
must update `PINNED_HASH` in the same PR. It doesn't state the reverse,
and nothing in the repo enforced it until #214 filed it: **a
`PINNED_HASH` change invalidates every reference trace keyed to the old
hash**, not just the ROM artifact itself. `refemu/reference_traces/`
names its files by the hash they were generated against precisely
because they're specific to one build
(`demo-boot-to-first-frame.<hash>.tsv`, `demo3.<hash>.json` + its
gitignored `.tsv`) — but a stale trace doesn't error at the point the
hash moves. It errors later, whenever a run actually walks past the old
trace's coverage.

#175/#198 (this file's own most recent `PINNED_HASH` move, the dormant-
unroll optimization) is the concrete instance: the boot-to-first-frame
trace was regenerated against the new hash and the demo3 trace's
manifest was re-measured and confirmed (#202) — but the demo3 trace's
own `.tsv` was never regenerated, because nothing connected "PINNED_HASH
moved" to "regenerate every trace keyed to the old one." That gap sat
invisible until a live milestone run reached the old trace's coverage
boundary mid-run and failed loudly (#214) — the right failure mode,
just a much later and more expensive point to discover it at than the
PR that moved the hash.

**Treat a `PINNED_HASH` change as incomplete until every reference trace
keyed to the old hash has a keyed-to-the-new-hash successor**, generated
or at least explicitly deferred with a tracking issue — not merely
"didn't come up in this PR's own testing." #214 tracks the two
operational fixes this should eventually make automatic (a `just`
recipe for the demo3 trace generator, and an upfront trace-coverage
check in the milestone runner so a short trace fails in under a second
instead of after however much compute a real run has already spent).

### A real P0, found by this exact check on its first real run

`PINNED_HASH` failed CI the moment it landed — not a flake. CI's build
(`22113f55...`) didn't match the hash I'd pinned locally on an Apple
Silicon Mac (`67fa83e1...`). Every previous "verified reproducible" claim
in this workstream's history (every prior PR back to #5) had only ever
compared a build against *itself*, repeatedly, on the same machine —
same host, same filesystem, every time. `PINNED_HASH` was the first check
that ever compared a build from one host against a build from a
genuinely different one, and it found two real, independent bugs sitting
underneath a reproducibility claim that had looked solid for five PRs:

1. **`$(wildcard $(DG_DIR)/*.c)` has host-filesystem-dependent order.**
   `make` expands `$(wildcard)` once, on whatever OS invoked it, *before*
   the file list ever reaches the container — it was never a
   container-side concern. macOS happened to return these sorted; Linux
   (every CI runner, and a QEMU-emulated Linux container tried locally)
   does not. Different source order changes where each translation unit's
   compiled code/data lands in the linked binary. Fixed with `$(sort
   ...)` (`rom/Makefile`) — a one-line fix for a difference that would
   otherwise have been invisible on any single developer's machine
   forever, only ever surfacing as "CI disagrees with my local build" with
   no other symptom.
2. **xpack's linux-arm64 and linux-x64 toolchain packages are not
   byte-identical, even after (1).** With source order fixed, an
   arm64-hosted build and an amd64-hosted build of the *identical* file
   set still differed — 182 bytes, in one place. Isolated by diffing the
   two `.bin` files directly: the difference is an embedded `__FILE__`
   path from an `assert()` in newlib's `dtoa.c` (double-to-string
   conversion), baked into the prebuilt `libc.a` each toolchain package
   ships — literally `.../build/linux-arm64/sources/newlib-.../dtoa.c` vs
   `.../build/linux-x64/sources/newlib-.../dtoa.c`, xpack's own build-host
   path from when *they* compiled each platform's release. Inert (the
   `assert()` never fires in this program) but real: two different
   physical `libc.a` files, embedding two different diagnostic strings,
   linked into our binary. No amount of sorting our own file list fixes
   this — it's not our nondeterminism, it's a fact about which prebuilt
   library gets linked. Fixed by pinning the **container platform**, not
   just the image digest: `rom/Makefile`'s `PLATFORM := linux/amd64` and
   `--platform $(PLATFORM)` on every `docker build`/`docker run` means the
   ROM is always compiled against the literal same `libc.a`, regardless of
   which CPU architecture `docker` itself is running on. Slower under
   emulation (e.g. on Apple Silicon, via QEMU) than a native build would
   be; never wrong, which is the actual requirement.

Verified the fix closes both gaps, not just CI's specific symptom: same
sha256 from two genuinely different execution environments — an Apple
Silicon Mac (`make`'s `docker run --platform linux/amd64` forces Docker
Desktop's Linux VM to run the container under QEMU emulation there) and
CI's real amd64 runner (no emulation, the container's native
architecture). Corrected from an earlier draft of this note that counted
the Mac build twice — once as "native arm64-macOS," once as
"QEMU-emulated-amd64, same Mac" — as if they were separate trials; they're
the same invocation of the same pinned pipeline on the same host, so it
was one data point double-counted, not three independent ones. Caught by
`refemu`'s review of the PR that introduced this note (#71) — worth
recording since this section exists specifically so a future reader can
trust its counts. This is also why the
top-of-file comment in `rom/Makefile` no longer claims "reproduces
byte-for-byte on any host with Docker" as a blanket statement without the
platform pin backing it — the claim is true now because something
specific (host, arch, container platform) makes it true, not because nothing
had yet contradicted it.
