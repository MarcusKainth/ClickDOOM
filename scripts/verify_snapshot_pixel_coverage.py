#!/usr/bin/env python3
"""Verify that FRAMEBUFFER/PALETTE are fully, freshly written between a
candidate snapshot seed icount and a target icount -- issue #229's
evidence route needs this, and so will every future one that reuses it.

## Why this exists

`rom/bench/canonical_throughput/{gen,seed}_snapshot.py` seed a resumed SQL
run's `ram`/`batch_commit` from a real, refemu-verified state at some
icount, deliberately WITHOUT restoring FRAMEBUFFER/PALETTE
(`gen_snapshot.py`'s own docstring: that MMIO state isn't captured). A
frame-readout test that seeds a run this way starts both pixel regions at
all-zero, then relies on the replayed instructions between the seed point
and the target to write every word that matters before checking
`fb_hash`. Whether that holds depends entirely on what the ROM actually
does in that specific window -- it is not something to assume from "the
regions are usually written every frame" (PALETTE especially: SPEC §2 and
`sqlcpu/schema.sql` both note palette writes are rare, on-change only, so
a short window ending anywhere but a frame boundary that happens to
rewrite the palette will leave it stale at all-zero).

**With #220 merged, a stale/undercovered region no longer shortens the
read or errors -- it reads as a clean 0-filled array, and the resulting
`fb_hash` is simply wrong, not obviously broken.** This script is the
check that stands between "this seed point is valid" and "we assumed it
was" -- run it before trusting a seeded-resume evidence run's `fb_hash`
against any oracle.

## The milestone-0 caveat this script exists to prevent silently spreading

Issue #229's own evidence run picked seed icount 15,333,136 / target
15,393,136 (frame 0's `FRAME_COMMIT`) and got full coverage on both
regions -- but **that result is specific to frame 0, not a general
property of this technique**: frame 0 is DOOM's first frame ever drawn,
so the program's first-ever palette write necessarily happens shortly
before that specific commit -- there is no earlier palette state for it
to be stale relative to. A later frame (25, 1,000, whatever) has no such
guarantee; its palette was very likely set once at startup, long before
any short seeded window ending at that frame's commit. **Anyone choosing
a new seed point for a different frame must re-run this check, not
assume #229's result carries over.** That is the entire reason this is a
committed, reusable tool and not a one-off scratch script.

## Determinism (SPEC §8)

Nothing on a result path reads a host clock or randomness; the only
`time.monotonic()` use would be a stderr progress ticker, and this script
doesn't even need one (the whole run is a few seconds at refemu speed).

## Method

Steps a fresh refemu CPU from icount 0 to `--seed-icount` unrecorded, then
instruments `Memory.write` to record every distinct FRAMEBUFFER/PALETTE
word address touched while stepping on to `--target-icount`. Reports
distinct words written against `driver/render.py`'s own
FRAMEBUFFER_WORDS/PALETTE_WORDS constants -- full coverage means every
word in the region was written at least once in the window, which is
what a zero-initialized seeded resume needs to reproduce the SAME content
a live run would have at the target icount.

Instruments by monkeypatching `Memory.write` at the class level (restored
in a `finally` block) rather than subclassing `Memory` -- `new_cpu()`
constructs its own `Memory` internally with no injection point for a
custom subclass, and this script has no reason to touch either module
just to add one.

If `--expect-fbhash` is given, also computes the ACTUAL `fb_hash` at the
target icount from this same run (via `refemu.trace.fb_hash()`, never a
second hash implementation) and asserts it matches -- this is what turns
"we counted some writes" into evidence: it proves the instrumentation
didn't perturb execution, by checking the instrumented run still
reproduces a value known correct from an uninstrumented run.

## `--seed-has-pixels` (#251)

Before #251, `gen_snapshot.py`/`seed_snapshot.py` didn't round-trip
FRAMEBUFFER/PALETTE at all, so a seeded resume always started both regions
at all-zero -- full window coverage (every word rewritten between seed and
target) was the ONLY way a seeded resume's `fb_hash` could be trusted, and
that is what this script checks by default (`--seed-has-pixels` unset):
incomplete coverage of either region is a hard failure.

#251 makes the seed itself carry real, correct FRAMEBUFFER/PALETTE content
at `--seed-icount`. With that in place, a word the window never rewrites
is no longer wrong -- it carries forward the seed's own correct value,
which is exactly what a live run would also have at that word (nothing
else in the ROM writes either region between two DG_DrawFrame calls,
confirmed in `driver/render.py`'s `frame_readout_sql()` docstring against
`rom/src/dg_hooks.c`). Full coverage stops being a *requirement* for
correctness and becomes purely informational: pass `--seed-has-pixels`
to report the same two coverage numbers without failing the run over an
incomplete one -- only `--expect-fbhash` (if given) still gates success.
Omit it to keep this script's original, stricter meaning (still the right
check for a caller that has NOT restored pixel state into its seed, or
that specifically wants to confirm a window is self-sufficient regardless
of what the seed carries).

Usage:
    cd refemu && uv run python ../scripts/verify_snapshot_pixel_coverage.py \\
        --seed-icount 15333136 --target-icount 15393136 \\
        --expect-fbhash fe5d82c0f42d45f1

    # A later frame, seeded with a #251-format snapshot that already
    # carries real pixel state -- incomplete window coverage is expected
    # and fine (e.g. the palette not changing that frame), so it must not
    # fail the run:
    cd refemu && uv run python ../scripts/verify_snapshot_pixel_coverage.py \\
        --seed-icount 221579723 --target-icount 221639723 \\
        --seed-has-pixels --expect-fbhash aa27f0470c7c5f3a
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(REPO / "refemu" / "src"))
sys.path.insert(0, str(REPO / "driver"))

import render  # FRAMEBUFFER_WORDS / PALETTE_WORDS -- SPEC §2, not re-declared here
from refemu.cpu import Halted, new_cpu
from refemu.memory import Memory
from refemu.trace import fb_hash

FRAMEBUFFER_BASE = 0x1100_0000
PALETTE_BASE = 0x1101_0000


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rom", default=str(REPO / "rom" / "build" / "doom-rv32im.bin"))
    ap.add_argument("--manifest", default=str(REPO / "rom" / "build" / "manifest.json"))
    ap.add_argument("--seed-icount", type=int, required=True)
    ap.add_argument("--target-icount", type=int, required=True)
    ap.add_argument("--expect-fbhash", default=None,
                     help="if given, assert the instrumented run's own fb_hash at --target-icount "
                          "matches this (16 lowercase hex digits) -- proves the instrumentation "
                          "didn't perturb execution, not just that some writes were counted")
    ap.add_argument("--seed-has-pixels", action="store_true",
                     help="(#251) the seed itself already carries real framebuffer/palette content "
                          "at --seed-icount (a format-2+ gen_snapshot.py/seed_snapshot.py snapshot), "
                          "so incomplete coverage in this window is expected/fine, not a failure -- "
                          "the coverage numbers are still printed, just not gating. Omit this for the "
                          "original, stricter meaning: a seed with NO pixel state, where full window "
                          "coverage is the only way the result can be trusted. See module docstring.")
    args = ap.parse_args()

    if args.target_icount <= args.seed_icount:
        print("::error::--target-icount must be strictly greater than --seed-icount", file=sys.stderr)
        return 1

    rom_path = Path(args.rom)
    manifest_path = Path(args.manifest)
    pinned = (REPO / "rom" / "PINNED_HASH").read_text().strip()
    image = rom_path.read_bytes()
    rom_sha256 = hashlib.sha256(image).hexdigest()
    if rom_sha256 != pinned:
        print(f"::error::{rom_path}: sha256 {rom_sha256} != PINNED_HASH {pinned} -- "
              f"refusing to verify coverage against an unpinned ROM", file=sys.stderr)
        return 1
    manifest = json.loads(manifest_path.read_text())

    cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
    cpu.memory.load_image(image, base=manifest["load_addr"])
    cpu.pc = manifest["load_addr"]

    fb_written: set[int] = set()
    pal_written: set[int] = set()
    recording = False

    orig_write = Memory.write

    def tracking_write(self: Memory, addr: int, width: int, value: int) -> None:
        if recording:
            if FRAMEBUFFER_BASE <= addr < FRAMEBUFFER_BASE + render.FRAMEBUFFER_WORDS * 4:
                fb_written.add((addr - FRAMEBUFFER_BASE) // 4)
            elif PALETTE_BASE <= addr < PALETTE_BASE + render.PALETTE_WORDS * 4:
                pal_written.add((addr - PALETTE_BASE) // 4)
        return orig_write(self, addr, width, value)

    Memory.write = tracking_write  # type: ignore[method-assign]
    try:
        while cpu.icount < args.seed_icount:
            try:
                cpu.step()
            except Halted as h:
                print(f"::error::CPU halted (reason={h.reason}) at icount={cpu.icount}, "
                      f"short of --seed-icount={args.seed_icount}", file=sys.stderr)
                return 1
        print(f"# reached seed icount={cpu.icount:,} (unrecorded)", file=sys.stderr)

        recording = True
        while cpu.icount < args.target_icount:
            try:
                cpu.step()
            except Halted as h:
                print(f"::error::CPU halted (reason={h.reason}) at icount={cpu.icount}, "
                      f"short of --target-icount={args.target_icount}", file=sys.stderr)
                return 1
        print(f"# reached target icount={cpu.icount:,} (recorded)", file=sys.stderr)

        actual_hash = f"{fb_hash(cpu.memory.framebuffer, cpu.memory.palette):016x}"
    finally:
        Memory.write = orig_write  # type: ignore[method-assign]

    fb_full = len(fb_written) == render.FRAMEBUFFER_WORDS
    pal_full = len(pal_written) == render.PALETTE_WORDS
    print(f"distinct_fb_words_written_in_window={len(fb_written)}/{render.FRAMEBUFFER_WORDS}")
    print(f"distinct_pal_words_written_in_window={len(pal_written)}/{render.PALETTE_WORDS}")
    print(f"actual_fbhash_at_target={actual_hash}")

    ok = True
    if not fb_full:
        if args.seed_has_pixels:
            print(f"# FRAMEBUFFER coverage incomplete: {len(fb_written)}/{render.FRAMEBUFFER_WORDS} words "
                  f"written in [{args.seed_icount}, {args.target_icount}) -- not a failure with "
                  f"--seed-has-pixels: the missing words carry forward the seed's own real value.",
                  file=sys.stderr)
        else:
            print(f"::error::FRAMEBUFFER coverage incomplete: {len(fb_written)}/{render.FRAMEBUFFER_WORDS} words "
                  f"written in [{args.seed_icount}, {args.target_icount}) -- a seeded resume from "
                  f"--seed-icount would read the missing words as 0 (post-#220) instead of their real value, "
                  f"producing a clean but WRONG fb_hash. (If your seed already carries real pixel state -- "
                  f"a #251-format snapshot -- pass --seed-has-pixels.)", file=sys.stderr)
            ok = False
    if not pal_full:
        if args.seed_has_pixels:
            print(f"# PALETTE coverage incomplete: {len(pal_written)}/{render.PALETTE_WORDS} words "
                  f"written in [{args.seed_icount}, {args.target_icount}) -- not a failure with "
                  f"--seed-has-pixels: the missing words carry forward the seed's own real value.",
                  file=sys.stderr)
        else:
            print(f"::error::PALETTE coverage incomplete: {len(pal_written)}/{render.PALETTE_WORDS} words "
                  f"written in [{args.seed_icount}, {args.target_icount}) -- same failure mode as above, "
                  f"and the one SPEC §2/schema.sql's own 'palette writes are rare' note predicts: don't "
                  f"assume a prior frame's coverage result carries over to this seed point. (If your seed "
                  f"already carries real pixel state -- a #251-format snapshot -- pass --seed-has-pixels.)",
                  file=sys.stderr)
            ok = False
    if not ok:
        return 1

    if args.expect_fbhash is not None and actual_hash != args.expect_fbhash:
        print(f"::error::instrumented run's own fb_hash={actual_hash} != --expect-fbhash="
              f"{args.expect_fbhash} -- the write-tracking instrumentation may have perturbed "
              f"execution; this result is not trustworthy as evidence until that's resolved.",
              file=sys.stderr)
        return 1

    if fb_full and pal_full:
        coverage_msg = "both regions fully written in the window"
    else:
        coverage_msg = ("coverage incomplete but not gating (--seed-has-pixels: the seed itself "
                         "supplies the rest)")
    print(f"coverage OK: {coverage_msg}"
          + (f", fb_hash matches --expect-fbhash={args.expect_fbhash}" if args.expect_fbhash else ""),
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
