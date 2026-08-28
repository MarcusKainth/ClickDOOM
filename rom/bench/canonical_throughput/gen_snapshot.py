"""Dump refemu's full CPU state (icount, pc, regs, RAM) at a target
instruction count, so `run.sh` can seed the SQL CPU's `ram`/`batch_commit`
directly at a representative point in the store-heavy gameplay window
instead of live-executing there.

## Why this exists

The gameplay window this benchmark measures (frames 200 -> 299 of
`-timedemo demo3`, per `rom/bench/e7_memfns`'s attribution against the
frozen ROM) starts at instruction 233,932,753. The SQL CPU runs at
roughly 1,000-2,000 instr/sec (ADR-0004), so live-executing to that point
before every benchmark run would cost tens of hours -- not something a
5-day sprint can pay repeatedly. refemu runs the same ROM at ~0.9M
instr/sec (rom/bench/e7_memfns/README.md), so reaching that icount here
costs minutes, not hours. This script does exactly that once, dumps the
full state, and `run.sh` reuses the dump on every subsequent invocation
until the ROM changes.

## Determinism (SPEC §8) and correctness

Nothing here reads a host clock or randomness on a path that affects the
dump's content (the only `time.monotonic()` use is a stderr progress
ticker). The ROM's sha256 is checked against `rom/PINNED_HASH`
unconditionally and fatally, matching `gen_reference_trace.py`'s
convention (an unasserted hash is exactly the class of bug that let PR
#100 review a stale ROM) -- NOT `profile_memfns.py`'s informational-only
MISMATCH print, because this dump seeds a measurement that would silently
report a real number for the wrong binary.

If the CPU halts before reaching the target icount, that is fatal too: a
snapshot mid-halt is not the "store-heavy gameplay" state the target
icount was chosen to represent, and silently returning it anyway would
make every throughput number downstream measure something other than
what it claims to. (`FRAME_COMMIT`s firing along the way are expected and
harmless -- the target icount for the gameplay window is itself chosen as
a frame-commit instant, frame 200 of 300, per `rom/bench/e7_memfns`; by
233M instructions in, roughly 200 frames have already committed.)

## What IS captured, since #251, and what still is not

`framebuffer`/`palette` (SPEC §2) are captured as of format version 2
(`snapshot_format.py`) -- `cpu.memory.framebuffer`/`.palette`, the same
dense, region-relative bytearrays `refemu.memory.Memory` already keeps
(byte 0 = the region's own base, not RAM_BASE/an absolute address). #251
filed this as a gap: a database seeded without them starts both pixel
regions at all-zero, so any frame-verification run whose replayed window
doesn't happen to rewrite every word (PALETTE especially -- SPEC §2 and
`sqlcpu/schema.sql` both note palette writes are rare, on-change only)
produces a clean, plausible, WRONG `fb_hash` with no error anywhere
(`scripts/verify_snapshot_pixel_coverage.py` is the tool that first made
this observable, measuring 0/192 palette words touched in the window
before frame 220). Capturing them here, restoring them in
`seed_snapshot.py`, is the fix.

`console_out`/`key_queue` remain uncaptured -- still no representation in
`sqlcpu/schema.sql` needed for a throughput measurement or a `fb_hash`
frame-verification run (neither reads console bytes or the key queue), so
there's nothing yet pulling this tool toward capturing them too. `run.sh`
seeds the ClickHouse side with placeholder MMIO columns for those two
(empty write-log, `keyq_pos=0`, `has_frame=0`) accordingly, documented
there.

## Caching

Written to `<out-dir>/snapshot.<rom sha256 prefix>.<icount>.v<format
version>.pkl`, atomically (`tmp` + `os.replace`, same pattern as
`refemu/scripts/gen_demo3_trace.py`'s `save_state`) -- a crash mid-write
leaves no half-written file for `run.sh` to mistake for a good one. The
ROM hash and target icount are both in the filename, so a stale snapshot
from a since-superseded ROM (or a different window) can never be silently
reused -- the same reasoning `gen_reference_trace.py`'s `default_out_path`
documents for its own hash-prefixed trace filenames. The format version
(`snapshot_format.FORMAT_VERSION`) is in the filename too, since #251: a
pre-#251 (format 1) cache sitting in `--out-dir` from a previous run has a
different filename under format 2, so it can never be mistaken for a
current-format snapshot and silently reused with an empty
`framebuffer`/`palette` -- the cache key changing is what forces
regeneration, on top of `seed_snapshot.py`'s own explicit
`format_version` check on the dict itself (belt and suspenders: the
filename protects against a stale file being found by a fresh run of this
script, the in-dict field protects against a stale file being handed to
`seed_snapshot.py` directly under any name).

Usage:
    cd refemu && uv run python ../rom/bench/canonical_throughput/gen_snapshot.py \\
        --target-icount 233932753 --out-dir /tmp/clickdoom-snapshots
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pickle
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROM_DIR = HERE.parent.parent  # rom/
REPO = ROM_DIR.parent
sys.path.insert(0, str(REPO / "refemu" / "src"))
sys.path.insert(0, str(HERE))

from refemu.cpu import Halted, new_cpu  # noqa: E402
from refemu.memory import FRAMEBUFFER_SIZE, PALETTE_SIZE  # noqa: E402
from snapshot_format import FORMAT_VERSION  # noqa: E402


def snapshot_path(out_dir: Path, rom_sha256: str, target_icount: int) -> Path:
    return out_dir / f"snapshot.{rom_sha256[:12]}.{target_icount}.v{FORMAT_VERSION}.pkl"


def generate(image: bytes, manifest: dict, target_icount: int) -> dict:
    """Run refemu to exactly `target_icount` retired instructions and
    return the state dict `run.sh`'s seeder needs. Raises RuntimeError on
    any halt or FRAME_COMMIT before the target -- see module docstring."""
    cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
    cpu.memory.load_image(image, base=manifest["load_addr"])
    cpu.pc = manifest["load_addr"]

    t0 = time.monotonic()
    last_tick = t0
    while cpu.icount < target_icount:
        try:
            cpu.step()
        except Halted as h:
            raise RuntimeError(
                f"CPU halted (reason={h.reason}, pc={h.pc:#x}) at icount={cpu.icount}, "
                f"short of target icount={target_icount} -- this window's target icount "
                f"no longer lands inside a live run of this ROM. Re-derive it from "
                f"rom/bench/e7_memfns against the current PINNED_HASH before retrying."
            ) from None
        now = time.monotonic()
        if now - last_tick > 5.0:
            print(f"# icount={cpu.icount:,} / {target_icount:,} "
                  f"({100.0 * cpu.icount / target_icount:.1f}%, "
                  f"{cpu.icount / max(now - t0, 1e-9):,.0f} instr/sec)", file=sys.stderr)
            last_tick = now

    elapsed = time.monotonic() - t0
    print(f"# reached icount={cpu.icount:,} in {elapsed:.1f}s "
          f"({cpu.icount / max(elapsed, 1e-9):,.0f} instr/sec)", file=sys.stderr)

    # framebuffer/palette (#251): refemu's Memory keeps both as dense,
    # already-region-relative bytearrays (byte 0 = the region's own base --
    # see memory.py's write() subtracting FRAMEBUFFER_BASE/PALETTE_BASE
    # before indexing), fixed at FRAMEBUFFER_SIZE/PALETTE_SIZE from
    # construction regardless of what's been written -- same "dense from
    # construction, no separate zero-fill step" property `ram` has, so
    # these need no sparse/dense reconciliation the way `ram`'s TSV load
    # in seed_snapshot.py briefly worried about for byte-range coverage.
    fb_bytes = bytes(cpu.memory.framebuffer)
    pal_bytes = bytes(cpu.memory.palette)
    assert len(fb_bytes) == FRAMEBUFFER_SIZE, (
        f"cpu.memory.framebuffer is {len(fb_bytes)} bytes, expected {FRAMEBUFFER_SIZE} "
        f"(SPEC §2) -- refemu.memory.Memory's own invariant, should be unreachable"
    )
    assert len(pal_bytes) == PALETTE_SIZE, (
        f"cpu.memory.palette is {len(pal_bytes)} bytes, expected {PALETTE_SIZE} "
        f"(SPEC §2) -- refemu.memory.Memory's own invariant, should be unreachable"
    )

    return {
        "format_version": FORMAT_VERSION,
        "icount": cpu.icount,
        "pc": cpu.pc,
        "regs": list(cpu.regs),  # 32 elements, regs[0] always 0 (SPEC §1)
        "ram": bytes(cpu.memory.ram),
        "ram_base": manifest["load_addr"],
        "framebuffer": fb_bytes,
        "palette": pal_bytes,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rom", default=str(ROM_DIR / "build" / "doom-rv32im.bin"))
    ap.add_argument("--manifest", default=str(ROM_DIR / "build" / "manifest.json"))
    ap.add_argument("--target-icount", type=int, required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--force", action="store_true", help="regenerate even if a cached snapshot exists")
    args = ap.parse_args()

    rom_path = Path(args.rom)
    image = rom_path.read_bytes()
    rom_sha256 = hashlib.sha256(image).hexdigest()
    pinned = (ROM_DIR / "PINNED_HASH").read_text().strip()
    if rom_sha256 != pinned:
        print(f"::error::{rom_path}: sha256 {rom_sha256} != PINNED_HASH {pinned} -- "
              f"refusing to snapshot an unpinned ROM (a throughput number measured "
              f"against the wrong binary is worse than no number)", file=sys.stderr)
        return 1

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = snapshot_path(out_dir, rom_sha256, args.target_icount)
    if out_path.exists() and not args.force:
        print(f"# reusing cached snapshot: {out_path}", file=sys.stderr)
        return 0

    manifest = json.loads(Path(args.manifest).read_text())
    print(f"# generating snapshot at icount={args.target_icount:,} "
          f"(rom={rom_sha256[:12]}) -- this runs refemu at ~0.9M instr/sec, "
          f"expect a few minutes", file=sys.stderr)
    state = generate(image, manifest, args.target_icount)
    state["rom_sha256"] = rom_sha256

    tmp_path = out_path.with_suffix(out_path.suffix + ".tmp")
    with open(tmp_path, "wb") as f:
        pickle.dump(state, f, protocol=pickle.HIGHEST_PROTOCOL)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp_path, out_path)
    print(f"# wrote {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
