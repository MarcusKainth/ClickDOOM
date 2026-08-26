#!/usr/bin/env python3
"""Generate the SPEC §7 reference trace for the real DOOM ROM, out to (at
least) the first `FRAME_COMMIT` -- issue #96's team-lead ask: a stored,
committed oracle Phase 2's milestone run diffs against, so a divergence at
instruction 4,000,000 is caught at instruction 4,000,000, not after ~2.8
hours of compute produces a final hash that doesn't match.

Run from the repo root (needs `rom/build/doom-rv32im.bin` +
`rom/build/manifest.json` -- build them first with `just build-rom`; this
script does not invoke the toolchain itself, since building the ROM is
`rom`'s job, not `refemu`'s):

    uv run --project refemu python refemu/scripts/gen_reference_trace.py

Output filenames default to `refemu/reference_traces/demo-boot-to-first-
frame.<rom sha256 prefix>.tsv`/`.json` -- the ROM's own hash prefix is
embedded in the filename itself, not only recorded inside the `.json`
sidecar, so a trace generated against one ROM can never be mistaken for
current after `rom/PINNED_HASH` moves (issue #96's own incident: this
script's first real output was generated against the attract-mode ROM
hours before the timedemo-argv ROM, #111, made it stale -- caught by a
teammate noticing a number in a status message, not by anything in this
repo, which is exactly the gap this convention closes). See the `.json`
file's own `generated_by` field for the exact command line used.

## Why this generates its own periodic-checkpoint loop instead of calling
## `refemu.trace.iter_trace()` directly

`iter_trace()` drives `cpu.step()` internally and only yields at
`CHECKPOINT_INTERVAL` boundaries, which is too coarse to capture the exact
instruction the first `FRAME_COMMIT` lands on, or the exact instruction
`I_InitGraphics`'s console line first appears -- both need per-step
observation. So this script steps the CPU itself and reproduces
`iter_trace()`'s exact periodic-checkpoint conditional inline, using
`iter_trace`'s own interval constants and hash functions (`CHECKPOINT_
INTERVAL`, `RAM_HASH_INTERVAL`, `reg_hash`, `ram_hash`, `fb_hash`,
`format_checkpoint` -- imported, never reimplemented), rather than
inventing a second trace format. `test_gen_reference_trace.py` proves this
inline loop produces line-for-line identical output to `run_trace()` on a
smaller instruction count, so "this script's checkpoints" and "the real
SPEC §7 emitter's checkpoints" are verified to be the same thing, not two
implementations that happen to agree today.

## The milestone checkpoints are a deliberate departure from SPEC §7

Both milestones below (I_InitGraphics, first FRAME_COMMIT) fall at
instruction counts that are NOT `CHECKPOINT_INTERVAL`/`RAM_HASH_INTERVAL`
multiples -- they are console-output/MMIO events, not periodic sampling
points, and SPEC §7 does not define (and this script does not propose) a
checkpoint format triggered by them. They are recorded in the companion
`.json` metadata file as diagnostic milestones, computed with the exact
same hash functions as the periodic trace, but are NOT lines in the `.tsv`
trace file -- a differential comparison should only ever compare `.tsv`
lines against `.tsv` lines of the same cadence.

## What would make this produce a plausible but wrong reference

The one class of bug that matters most here: this script silently drifting
from what `refemu.trace`'s real checkpoint emitter would produce (see the
loop-equivalence test above), or asserting the ROM's hash without actually
checking it (silently validating a different binary than `PINNED_HASH`
pins -- exactly what bit PR #100's review before it re-verified from
scratch, see that PR's thread). Both are guarded explicitly below: the
sha256 assertion is unconditional and fatal, and every milestone this
script reports is either cross-checked against issue #29's independently
reproduced numbers (when `--expect-*` flags are given, the default) or
printed loudly as a mismatch, never silently accepted.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from refemu.cpu import Halted, new_cpu
from refemu.memory import RAM_BASE
from refemu.trace import (
    CHECKPOINT_INTERVAL,
    RAM_HASH_INTERVAL,
    fb_hash,
    format_checkpoint,
    ram_hash,
    reg_hash,
)

sys.path.insert(0, str(Path(__file__).resolve().parent))
from _rom_provenance import UnpinnedRomError, assert_pinned_hash, hashed_filename

INIT_GRAPHICS_NEEDLE = b"I_InitGraphics: framebuffer"

REPO_ROOT = Path(__file__).resolve().parent.parent.parent


def default_out_path(repo_root: Path, rom_sha256: str) -> Path:
    """The trace file's own name records which ROM it was generated
    against -- via `_rom_provenance.hashed_filename` (issue #129's follow-
    up: this used to be its own inline `f"...{rom_sha256[:12]}..."`,
    duplicating exactly the discipline `gen_demo3_trace.py` needed too;
    now both scripts share one implementation, so they can't drift on the
    hash-prefix length or the filename shape)."""
    return repo_root / "refemu" / "reference_traces" / hashed_filename("demo-boot-to-first-frame", rom_sha256, ".tsv")


def generate(image: bytes, manifest: dict, max_instructions: int) -> dict:
    """Run the real ROM through refemu, collecting the SPEC §7 periodic
    trace plus the I_InitGraphics/FRAME_COMMIT milestones. Returns a dict
    with `lines` (the .tsv content, as a list of strings) and `meta` (the
    .json content, minus the fields the caller fills in)."""
    cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
    cpu.memory.load_image(image, base=RAM_BASE)
    cpu.pc = RAM_BASE

    lines: list[str] = []
    last_console_len = 0
    last_console_change_icount: int | None = None
    init_graphics_icount: int | None = None
    frame_commit: dict | None = None
    halt_info: dict | None = None

    t0 = time.monotonic()
    while cpu.icount < max_instructions:
        try:
            cpu.step()
        except Halted as h:
            halt_info = {
                "reason": h.reason,
                "pc": h.pc,
                "icount": cpu.icount,
                "insn": h.insn,
                "addr": h.addr,
                "exit_code": h.exit_code,
            }
            break

        # I_InitGraphics milestone: NOT "when the needle first appears" --
        # PUTCHAR appends one byte at a time, so the needle completes
        # mid-print, well before the console settles. The milestone that
        # matches issue #29's number is "the last console byte written
        # before the first FRAME_COMMIT" -- confirmed, not assumed: this
        # ROM prints nothing else between I_InitGraphics's block finishing
        # and the first frame, so tracking "last console change observed
        # by the time FRAME_COMMIT fires" and requiring the needle be
        # present in the final text is the same instant as "I_InitGraphics
        # reached" for this ROM, without hardcoding that assumption for a
        # future one -- a ROM that prints something else after
        # I_InitGraphics before its first frame would (correctly) report
        # that later console activity's icount instead, not silently keep
        # matching an assumption that no longer holds.
        console = cpu.memory.mmio.console_out
        if len(console) != last_console_len:
            last_console_len = len(console)
            last_console_change_icount = cpu.icount

        if frame_commit is None and cpu.memory.mmio.frame_commits:
            if last_console_change_icount is not None and INIT_GRAPHICS_NEEDLE in bytes(console):
                init_graphics_icount = last_console_change_icount
            frame_no, committed_icount = cpu.memory.mmio.frame_commits[0]
            frame_commit = {
                "frame_no": frame_no,
                # Mmio.frame_commits' own committed_icount is "icount
                # before the store itself" by construction (see mmio.py);
                # cpu.icount here is the checkpoint-style convention
                # (total instructions retired, i.e. one more) -- both
                # recorded so the one-off relationship is visible, not
                # just asserted in a comment.
                "committed_icount": committed_icount,
                "icount": cpu.icount,
                "pc": cpu.pc,
                "checkpoint": format_checkpoint(
                    cpu.icount,
                    cpu.pc,
                    reg_hash(cpu.pc, cpu.regs),
                    ram_hash(cpu.memory.ram),
                    fb_hash(cpu.memory.framebuffer, cpu.memory.palette),
                ),
            }

        if cpu.icount % CHECKPOINT_INTERVAL == 0:
            rh = reg_hash(cpu.pc, cpu.regs)
            at_ram_interval = cpu.icount % RAM_HASH_INTERVAL == 0
            ramh = ram_hash(cpu.memory.ram) if at_ram_interval else None
            fbh = fb_hash(cpu.memory.framebuffer, cpu.memory.palette) if at_ram_interval else None
            lines.append(format_checkpoint(cpu.icount, cpu.pc, rh, ramh, fbh))
    elapsed = time.monotonic() - t0

    return {
        "lines": lines,
        "meta": {
            "final_icount": cpu.icount,
            "halt": halt_info,
            "init_graphics_icount": init_graphics_icount,
            "frame_commit": frame_commit,
            "checkpoint_interval": CHECKPOINT_INTERVAL,
            "ram_hash_interval": RAM_HASH_INTERVAL,
            "generation_seconds": round(elapsed, 3),
            "instructions_per_second": round(cpu.icount / elapsed) if elapsed > 0 else None,
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(REPO_ROOT / "rom" / "build" / "doom-rv32im.bin"))
    parser.add_argument("--manifest", default=str(REPO_ROOT / "rom" / "build" / "manifest.json"))
    parser.add_argument("--pinned-hash", default=str(REPO_ROOT / "rom" / "PINNED_HASH"))
    parser.add_argument(
        "--max-instructions",
        type=int,
        default=15_728_640,  # 15 * RAM_HASH_INTERVAL: the first full
        # ram/fb-hash checkpoint at or past the first FRAME_COMMIT (icount
        # 15,653,137 as of #127's DG_DrawFrame unroll, previously
        # 15,695,836 pre-#127 -- both fall in [14, 15) * RAM_HASH_INTERVAL,
        # so this constant didn't need to move with that ROM change), for
        # margin -- not a magic number, see module docstring.
    )
    parser.add_argument(
        "--out",
        default=None,
        help=(
            "default: refemu/reference_traces/demo-boot-to-first-frame.<rom sha256"
            " prefix>.tsv -- computed after the ROM is hashed below (issue #96's"
            " incident: a trace generated against one ROM getting silently treated"
            " as current after the ROM changed underneath it -- embedding the hash"
            " in the filename itself, not just the .json sidecar, means a stale"
            " trace can never be mistaken for a fresh one by name alone)."
        ),
    )
    parser.add_argument("--out-meta", default=None, help="default: --out's path with .json instead of .tsv")
    # Cross-checked against issue #29's independently reproduced numbers by
    # default. Pass --no-expect to generate without asserting (e.g. against
    # a deliberately different ROM), but the default is to fail loudly on
    # a mismatch, not silently accept a different reference than the one
    # this script's docstring claims to produce.
    parser.add_argument("--expect-init-graphics-icount", type=int, default=11_016_543)
    parser.add_argument("--expect-frame-commit-icount", type=int, default=15_653_137)
    parser.add_argument(
        # Unchanged by #127's DG_DrawFrame unroll -- that's the whole point
        # (icount moved, fb_hash didn't; see #127's evidence and #29).
        "--expect-frame-commit-fbhash", default="fe5d82c0f42d45f1", help="hex, no 0x prefix"
    )
    parser.add_argument("--no-expect", action="store_true", help="skip the issue-#29 cross-check")
    args = parser.parse_args()

    image_path = Path(args.image)
    manifest_path = Path(args.manifest)
    pinned_hash_path = Path(args.pinned_hash)

    image = image_path.read_bytes()
    manifest = json.loads(manifest_path.read_text())
    try:
        actual = assert_pinned_hash(image, pinned_hash_path)
    except UnpinnedRomError as e:
        print(
            f"FATAL: {e}\n"
            "Refusing to generate a reference trace against an unpinned ROM -- "
            "rebuild with `just build-rom` from a clean checkout, or pass "
            "--pinned-hash if this is deliberate (e.g. reviewing a ROM PR).",
            file=sys.stderr,
        )
        return 1
    print(f"# ROM sha256 matches PINNED_HASH: {actual}", file=sys.stderr)

    # Computed here, not as an argparse default, because it depends on
    # `actual` -- the ROM's own hash, just verified above -- not on
    # anything known before that check runs.
    out_path = Path(args.out) if args.out else default_out_path(REPO_ROOT, actual)
    out_meta_path = Path(args.out_meta) if args.out_meta else out_path.with_suffix(".json")

    result = generate(image, manifest, args.max_instructions)
    meta = result["meta"]

    print(
        f"# generated {len(result['lines'])} checkpoint lines, "
        f"final icount={meta['final_icount']}, "
        f"{meta['instructions_per_second']} instr/sec "
        f"({meta['generation_seconds']}s)",
        file=sys.stderr,
    )

    mismatches = []
    if not args.no_expect:
        ig = meta["init_graphics_icount"]
        if ig != args.expect_init_graphics_icount:
            mismatches.append(
                f"I_InitGraphics icount: expected {args.expect_init_graphics_icount}, got {ig}"
            )
        fc = meta["frame_commit"]
        if fc is None:
            mismatches.append(
                f"first FRAME_COMMIT: expected at icount {args.expect_frame_commit_icount}, "
                f"never observed within --max-instructions={args.max_instructions}"
            )
        else:
            if fc["icount"] != args.expect_frame_commit_icount:
                mismatches.append(
                    f"first FRAME_COMMIT icount: expected {args.expect_frame_commit_icount}, "
                    f"got {fc['icount']}"
                )
            fbhash_hex = fc["checkpoint"].split("\t")[-1]
            if fbhash_hex != args.expect_frame_commit_fbhash:
                mismatches.append(
                    f"first FRAME_COMMIT fbhash: expected {args.expect_frame_commit_fbhash}, "
                    f"got {fbhash_hex}"
                )

    if mismatches:
        print("FATAL: generated reference disagrees with issue #29's numbers:", file=sys.stderr)
        for m in mismatches:
            print(f"  - {m}", file=sys.stderr)
        print(
            "Either the ROM genuinely changed (expected -- update --expect-* "
            "or pass --no-expect and investigate), or refemu itself diverged "
            "from its own prior measurement (not expected -- investigate "
            "before trusting this output).",
            file=sys.stderr,
        )
        return 1

    if meta["frame_commit"] is not None:
        print(f"# first FRAME_COMMIT: {meta['frame_commit']['checkpoint']}", file=sys.stderr)
    if meta["init_graphics_icount"] is not None:
        print(f"# I_InitGraphics reached at icount={meta['init_graphics_icount']}", file=sys.stderr)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(result["lines"]) + "\n")

    full_meta = {
        "spec_version": manifest.get("spec_version"),
        "rom_sha256": actual,
        "rom_manifest": manifest,
        "generated_by": "refemu/scripts/gen_reference_trace.py " + " ".join(sys.argv[1:]),
        "trace_file": out_path.name,
        "trace_line_count": len(result["lines"]),
        **meta,
    }
    out_meta_path.write_text(json.dumps(full_meta, indent=2) + "\n")

    print(f"# wrote {out_path} ({len(result['lines'])} lines)", file=sys.stderr)
    print(f"# wrote {out_meta_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
