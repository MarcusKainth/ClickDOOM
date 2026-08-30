# E7 — are `memcpy`/`memset` byte-loop shims, and what do they cost?

## The question

An external reviewer hypothesised that DOOM's renderer is memcpy-saturated
and that `rom/src/syscalls.c` provides naive byte-loop `memcpy`/`memset`
shims costing ~4-6 emulated instructions per byte, where a word-wise
implementation costs ~1.5. If true, fixing the shim would cut the *total
instruction count* of a run (currently ~2.91e9 for `-timedemo demo3`),
which is a different lever from making each instruction cheaper.

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
adds nothing to the repository's Python dependencies.

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
about 170M instructions/sec, so 300 frames, which is about 330M
instructions, takes roughly four seconds.

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
