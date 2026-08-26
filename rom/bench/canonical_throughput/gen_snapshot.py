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

## What is NOT captured, and why that is fine here

`framebuffer`/`palette`/`console_out`/`key_queue` are refemu MMIO state
with no representation in `sqlcpu/schema.sql` yet (#144's own
`checkpoint_query.py` hit the same gap: FRAMEBUFFER/PALETTE SQL storage
doesn't exist, #130/#29). This tool only needs `pc`/`regs`/`ram`/`icount`
-- the state a batch of `arrayFold` steps actually reads -- to produce a
throughput measurement; it is not trying to reproduce the exact MMIO
continuity a real resumed run would have. `run.sh` seeds the ClickHouse
side with placeholder MMIO columns (empty write-log, `keyq_pos=0`,
`has_frame=0`) accordingly, documented there.

## Caching

Written to `<out-dir>/snapshot.<rom sha256 prefix>.<icount>.pkl`, atomically
(`tmp` + `os.replace`, same pattern as `refemu/scripts/gen_demo3_trace.py`'s
`save_state`) -- a crash mid-write leaves no half-written file for `run.sh`
to mistake for a good one. The ROM hash and target icount are both in the
filename, so a stale snapshot from a since-superseded ROM (or a different
window) can never be silently reused -- the same reasoning
`gen_reference_trace.py`'s `default_out_path` documents for its own
hash-prefixed trace filenames.

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

from refemu.cpu import Halted, new_cpu  # noqa: E402


def snapshot_path(out_dir: Path, rom_sha256: str, target_icount: int) -> Path:
    return out_dir / f"snapshot.{rom_sha256[:12]}.{target_icount}.pkl"


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

    return {
        "icount": cpu.icount,
        "pc": cpu.pc,
        "regs": list(cpu.regs),  # 32 elements, regs[0] always 0 (SPEC §1)
        "ram": bytes(cpu.memory.ram),
        "ram_base": manifest["load_addr"],
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
