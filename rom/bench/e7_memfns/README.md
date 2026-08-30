# E7 — are `memcpy`/`memset` byte-loop shims, and what do they cost?

## The question

An external reviewer hypothesised that DOOM's renderer is memcpy-saturated
and that `rom/src/syscalls.c` provides naive byte-loop `memcpy`/`memset`
shims costing ~4-6 emulated instructions per byte, where a word-wise
implementation costs ~1.5. If true, fixing the shim would cut the *total
instruction count* of a run (currently ~2.91e9 for `-timedemo demo3`),
which is a different lever from making each instruction cheaper.

## The answer (short)

**REJECT.** `rom/src/syscalls.c` does not define `memcpy`, `memset`,
`memmove` or any string routine — it only *calls* two of them. `rom/Makefile`
links the pinned toolchain's newlib (`-lc -lm -lgcc`), so `memcpy` and
`memset` are newlib's, and disassembly plus a full-attribution profile both
show they are already word-wise. See the GitHub issue for the numbers.

## What the harness does

`profile_memfns.py` runs `rom/build/doom-rv32im.bin` under `refemu` and
attributes **every** retired pc (not a sample) to an ELF `STT_FUNC` symbol,
so the output is an exact per-function instruction budget. On top of that it
traps the entry pc of `memcpy`/`memset`/`memmove`/`memcmp`/`strlen`/
`strcpy`/`strcmp`/`strncpy` and reads the ilp32 argument registers, giving
per-function call counts, total bytes requested, a power-of-two length
histogram, and — for the copy routines — the fraction of calls whose
operands are mutually word-aligned (`(src ^ dst) & 3 == 0`), which is the
condition newlib's fast path requires. Dividing instructions by bytes gives
the measured instructions-per-byte directly, with no modelling.

`elfsyms.py` is a dependency-free ELF32 symbol-table reader, so the harness
adds nothing to `refemu/pyproject.toml`.

Windows reported: the whole run, boot (0 → first `FRAME_COMMIT`), steady
state (first → last `FRAME_COMMIT`), plus any `--extra-window LO:HI` frame
ranges. **Boot must be read separately**: it contains crt0's `.bss`-zeroing
loop (`_start`, 184,048 instructions, all stores and no loads) and the
one-shot ~3.8 MB WAD load, both of which skew any whole-run histogram.

## Determinism

Nothing on a result path reads a host clock or any randomness. The only
`time.time()` call drives a stderr progress ticker. A run is a pure function
of (ROM image, manifest, `--frames`). `refemu`'s `TICKS_MS` is elastic
(SPEC §3.1: retired-instruction count / IPMS), so the emulated timeline is
reproducible too.

## Rerunning

    cd refemu
    uv run python ../rom/bench/e7_memfns/profile_memfns.py --frames 40
    uv run python ../rom/bench/e7_memfns/profile_memfns.py \
        --frames 300 --max-instructions 600000000 \
        --extra-window 40:170 --extra-window 200:299 \
        --json /tmp/e7_demo.json

Build the ROM first if `rom/build/` is empty (`make -C rom`). The script
prints the image's sha256 next to `rom/PINNED_HASH` and says MATCH or
MISMATCH — quote that hash with any number taken from it. Throughput is
about 0.9M instructions/sec, so 300 frames (~340M instructions) takes
roughly 6 minutes.

Disassembling the routines under test needs the pinned toolchain image:

    cd rom && docker run --rm --platform linux/amd64 -v "$PWD":/work -w /work \
      clickdoom-rom-toolchain:15.2.0-1 \
      riscv-none-elf-objdump -d --start-address=0x800493dc \
        --stop-address=0x800495e0 build/doom-rv32im.elf

## Committed

This directory is bench evidence for a question already answered (REJECT,
see above), and a generally reusable tool beyond that one question — exact
per-symbol instruction attribution against any ROM build, not just this
one's memcpy/memset question. Kept in the tree per the team lead's ask
rather than left to a `git clean`; run it via `make bench-e7-memfns` per
CLAUDE.md's "use make targets only."

## Evidence in `results/`

- `run-40frames.txt` — boot + 40 title-screen frames (31.6M instructions).
- `run-300frames.txt` / `.json` — 300 frames (307.4M instructions), reaching
  real demo playback around frame ~150. The window that matters is
  `frames 200 -> 299`: 147,292,608 instructions, ~1.47M per tic, which
  matches ADR-0004's independently measured ~1.36M instructions/tic for
  `demo3`. Frames 0-~150 are title screen / menu and are **not**
  representative — `V_DrawPatch` dominates there and `R_DrawColumn` /
  `R_DrawSpan` are absent.

Both runs were taken against ROM sha256
`22113f55234fa050dbd9ece64b8d713451b32ddc6b2b3f3c289c2bff87c955ed`
(matching `rom/PINNED_HASH` at the time). **That ROM was the boot-to-attract-mode
build** — before #107/#111 wired a fixed `-timedemo demo3` argv. It has since
been superseded three times (#100, #111, #125); the ROM this repo ships today
is a different binary running a different program (a specific recorded demo,
not the attract-mode loop).

## Re-run against the current, frozen ROM (`eabb12ed…`) — does it reproduce?

Re-ran with identical parameters (`--frames 300 --max-instructions 600000000
--extra-window 40:170 --extra-window 200:299`) against
`rom/PINNED_HASH eabb12ed4f188f456177fc11a1fdcf3046ee5c9c38c8d2fd33246c72bd2ab92c`
(the current, frozen `-timedemo demo3` ROM). Results: `results/run-300frames-eabb12ed.txt`/`.json`.

**The headline conclusion (REJECT the byte-loop-shim hypothesis) holds, and
matches closely**: `memcpy` in the `frames 200 -> 299` steady-state window is
**0.836 instr/byte** (was 0.838), still ~3.5% of instructions (was 3.81%). Not
a coincidence — `dg_hooks.c`/newlib's `memcpy` didn't change between these
ROMs, only the argv and (later) `DG_DrawFrame`'s loop shape did.

**`DG_DrawFrame`'s share dropped from 5.41% to 2.39%** of the same window —
expected, and a useful independent cross-check: #125's x8 unroll measured a
~53% reduction in that function's own cost, and 2.39/5.41 ≈ 0.44, i.e. a
~56% reduction here, consistent within the noise of comparing two different
windows (this run's frames 200-299 aren't the identical wall-clock content
the original run's frames 200-299 were, since the ROM plays a different
program now).

**What does *not* reproduce, and is worth knowing rather than glossing
over**: `R_DrawColumn` and `R_DrawSpan` have effectively **swapped rank** —
was `R_DrawColumn` 30.86% / `R_DrawSpan` 22.35%, now `R_DrawSpan` 34.68% /
`R_DrawColumn` 22.09%. This is not measurement noise or a harness bug — the
old run profiled whatever DOOM's attract-mode loop happened to render
(`DEMO1`/`DEMO2`/`DEMO3`/title, repeating); the new run profiles actual
`-timedemo demo3` playback, a specific recorded demo. Different rooms have
different wall-to-floor/ceiling proportions, so a wall-rasterizer
(`R_DrawColumn`) versus span-rasterizer (`R_DrawSpan`) ratio genuinely
depends on which scene is on screen — these are two different measurements
of two different programs, not two measurements of the same one. Anyone
citing "R_DrawColumn is ~31% of instructions" from the old `results/` file
should re-check against `run-300frames-eabb12ed.txt` instead; anyone citing
the memcpy conclusion or the `DG_DrawFrame` reduction is fine either way.
