"""Dump refemu's real FRAMEBUFFER/PALETTE bytes at the milestone icount
(#110's target, #29's oracle) for driver/render.py's fixture tests, so
render.py's readout query can be proven against real, known-correct data
without #160's real persistence pipeline existing yet.

## Why this, not rom/bench/canonical_throughput/gen_snapshot.py

That script dumps `pc`/`regs`/`ram` for a different purpose (seeding a
throughput benchmark's starting state) and doesn't touch FRAMEBUFFER/
PALETTE at all. This script is render-scope (driver/), dumps only
FRAMEBUFFER/PALETTE (plus the frame_no/committed_icount that fires
alongside them), and additionally verifies a real `FRAME_COMMIT` landed at
the target icount -- render.py's oracle test needs that pairing, not just
raw bytes at an arbitrary point.

## Determinism (SPEC §8)

Same as every other refemu-driving script in this repo: nothing on a
result path reads a host clock or randomness; the only `time.monotonic()`
use is a stderr progress ticker.

Usage:
    cd refemu && uv run python ../driver/gen_frame_fixture.py \\
        --target-icount 15653137 --out /tmp/frame_fixture.pkl
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pickle
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(REPO / "refemu" / "src"))

from refemu.cpu import Halted, new_cpu  # noqa: E402
from refemu.trace import fb_hash  # noqa: E402 -- the oracle, computed here too, not trusted blind


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rom", default=str(REPO / "rom" / "build" / "doom-rv32im.bin"))
    ap.add_argument("--manifest", default=str(REPO / "rom" / "build" / "manifest.json"))
    ap.add_argument("--target-icount", type=int, required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rom_path = Path(args.rom)
    image = rom_path.read_bytes()
    rom_sha256 = hashlib.sha256(image).hexdigest()
    pinned = (REPO / "rom" / "PINNED_HASH").read_text().strip()
    if rom_sha256 != pinned:
        print(f"::error::{rom_path}: sha256 {rom_sha256} != PINNED_HASH {pinned} -- "
              f"refusing to build a fixture against an unpinned ROM", file=sys.stderr)
        return 1

    manifest = json.loads(Path(args.manifest).read_text())
    cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
    cpu.memory.load_image(image, base=manifest["load_addr"])
    cpu.pc = manifest["load_addr"]

    t0 = time.monotonic()
    last_tick = t0
    while cpu.icount < args.target_icount:
        try:
            cpu.step()
        except Halted as h:
            print(f"::error::CPU halted (reason={h.reason}, pc={h.pc:#x}) at icount={cpu.icount}, "
                  f"short of target icount={args.target_icount}", file=sys.stderr)
            return 1
        now = time.monotonic()
        if now - last_tick > 5.0:
            print(f"# icount={cpu.icount:,} / {args.target_icount:,} "
                  f"({cpu.icount / max(now - t0, 1e-9):,.0f} instr/sec)", file=sys.stderr)
            last_tick = now

    elapsed = time.monotonic() - t0
    print(f"# reached icount={cpu.icount:,} in {elapsed:.1f}s", file=sys.stderr)

    # mmio.frame_commits records icount *before* the retiring FRAME_COMMIT
    # instruction's own cpu.icount += 1 (cpu.py's step(): _execute() runs
    # -- which is what calls mmio.write() -- strictly before the
    # increment), so its stored value is one less than the conventionally
    # cited milestone icount (#110's 15,653,137, e7_memfns's matching
    # "boot: 0 -> first FRAME_COMMIT (15,653,137 instructions)" window
    # size -- both post-increment: total instructions retired so far,
    # including the commit instruction itself). Found empirically running
    # this script (frame_commits[0] = (0, 15653136) against a target of
    # 15653137), not assumed -- ROM's actual behaviour didn't change, only
    # this check's convention needed fixing.
    frame_commits = cpu.memory.mmio.frame_commits
    matching = [fc for fc in frame_commits if fc[1] == args.target_icount - 1]
    if not matching:
        print(f"::error::no FRAME_COMMIT recorded at icount={args.target_icount - 1} "
              f"(one less than target={args.target_icount}, matching mmio.frame_commits' "
              f"pre-increment convention) -- frame_commits near target: "
              f"{[fc for fc in frame_commits if abs(fc[1] - args.target_icount) < 100]}. "
              f"This target icount no longer lands on a real commit for this ROM -- re-derive it "
              f"before trusting this fixture.", file=sys.stderr)
        return 1
    frame_no, _pre_increment_icount = matching[0]
    committed_icount = args.target_icount  # the post-increment, conventionally-cited number

    framebuffer = bytes(cpu.memory.framebuffer)
    palette = bytes(cpu.memory.palette)
    computed_fbhash = f"{fb_hash(framebuffer, palette):016x}"
    print(f"# frame_no={frame_no} committed_icount={committed_icount} "
          f"fbhash={computed_fbhash}", file=sys.stderr)

    state = {
        "rom_sha256": rom_sha256,
        "target_icount": args.target_icount,
        "frame_no": frame_no,
        "committed_icount": committed_icount,
        "framebuffer": framebuffer,
        "palette": palette,
        "fbhash": computed_fbhash,
    }
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = out_path.with_suffix(out_path.suffix + ".tmp")
    with open(tmp_path, "wb") as f:
        pickle.dump(state, f, protocol=pickle.HIGHEST_PROTOCOL)
    tmp_path.replace(out_path)
    print(f"# wrote {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
