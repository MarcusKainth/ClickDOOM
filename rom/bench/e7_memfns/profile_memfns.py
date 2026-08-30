"""E7: where do DOOM's emulated instructions actually go, and how much of
that is memcpy/memset?

Runs rom/build/doom-rv32im.bin under refemu, attributing *every* retired pc
to an ELF function symbol (not sampling -- exact counts), and additionally
instruments each call to memcpy/memset/memmove/strlen/... by trapping the
function's entry pc and reading the argument registers. That gives, per
function: instructions executed, calls made, bytes requested, and -- for
memcpy/memmove -- how many calls took the word-wise fast path
((src^dst)&3 == 0) versus the byte-at-a-time fallback.

Determinism (SPEC §8): nothing here reads a host clock or any source of
randomness on a path that affects a reported number. The only wall-clock
read is a progress ticker printed to stderr, which no result depends on.
The run is a pure function of (ROM image, manifest, --frames budget).

Usage (from refemu/, which owns the venv):

    cd refemu && uv run python ../rom/bench/e7_memfns/profile_memfns.py \\
        --frames 12 --json /tmp/e7.json

See README.md in this directory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import time
from bisect import bisect_right
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROM_DIR = HERE.parent.parent  # rom/
REPO = ROM_DIR.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "refemu" / "src"))

from elfsyms import read_func_symbols  # noqa: E402

from refemu.cpu import Halted, new_cpu  # noqa: E402

# Functions whose calls we instrument at entry. Argument registers are the
# RISC-V ilp32 ABI's: a0=x10, a1=x11, a2=x12.
#   name -> (dst_reg, src_reg, len_reg)  (None where the arg does not exist)
INSTRUMENTED = {
    "memcpy": (10, 11, 12),
    "memmove": (10, 11, 12),
    "memset": (10, None, 12),
    "memcmp": (10, 11, 12),
    "strlen": (10, None, None),
    "strcpy": (10, 11, None),
    "strcmp": (10, 11, None),
    "strncpy": (10, 11, 12),
}


def build_symbol_table(elf_path: Path):
    syms = read_func_symbols(elf_path)
    starts = [s[0] for s in syms]
    names = [s[2] for s in syms]
    ends = []
    for i, (addr, size, _name) in enumerate(syms):
        nxt = starts[i + 1] if i + 1 < len(starts) else addr + max(size, 4)
        ends.append(addr + size if size else nxt)
    return starts, ends, names


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rom", default=str(ROM_DIR / "build" / "doom-rv32im.bin"))
    ap.add_argument("--elf", default=str(ROM_DIR / "build" / "doom-rv32im.elf"))
    ap.add_argument("--manifest", default=str(ROM_DIR / "build" / "manifest.json"))
    ap.add_argument(
        "--frames",
        type=int,
        default=12,
        help="stop after this many FRAME_COMMITs (0 = stop at the first)",
    )
    ap.add_argument("--max-instructions", type=int, default=400_000_000)
    ap.add_argument("--json", help="write the full result as JSON here")
    ap.add_argument(
        "--extra-window",
        action="append",
        default=[],
        metavar="LO:HI",
        help="also report the window between FRAME_COMMIT LO and HI (frame indices, "
        "0-based). Use this to separate the title screen from actual demo playback.",
    )
    args = ap.parse_args()

    rom_path, elf_path = Path(args.rom), Path(args.elf)
    image = rom_path.read_bytes()
    rom_sha = hashlib.sha256(image).hexdigest()
    pinned = (ROM_DIR / "PINNED_HASH").read_text().strip()
    print(f"# rom      : {rom_path}")
    print(f"# rom sha256: {rom_sha}")
    print(f"# PINNED_HASH: {pinned}  ({'MATCH' if rom_sha == pinned else 'MISMATCH'})")

    manifest = json.loads(Path(args.manifest).read_text())
    starts, ends, names = build_symbol_table(elf_path)
    nsym = len(names)
    print(f"# symbols  : {nsym} STT_FUNC")

    entry_of = {}
    for i, nm in enumerate(names):
        if nm in INSTRUMENTED and nm not in entry_of:
            entry_of[starts[i]] = nm
    print(f"# instrumented entries: {sorted(entry_of.values())}")

    cpu = new_cpu(text_start=manifest["text_start"], text_end=manifest["text_end"])
    cpu.memory.load_image(image, base=manifest["load_addr"])
    cpu.pc = manifest["load_addr"]

    counts = [0] * (nsym + 1)  # last slot: pc outside every known symbol
    unknown = nsym
    # call stats: name -> dict
    calls = {
        nm: {"calls": 0, "bytes": 0, "aligned_calls": 0, "aligned_bytes": 0, "len_hist": {}}
        for nm in INSTRUMENTED
    }
    snapshots = []  # (label, icount, counts copy, calls copy)

    def snapshot(label):
        snapshots.append(
            (
                label,
                cpu.icount,
                counts[:],
                {k: {kk: (dict(vv) if isinstance(vv, dict) else vv) for kk, vv in v.items()} for k, v in calls.items()},
            )
        )

    fc = cpu.memory.mmio.frame_commits
    target_frames = max(args.frames, 1)
    budget = args.max_instructions
    halted = None
    t0 = time.time()
    next_tick = 5_000_000

    # -- hot loop --------------------------------------------------------
    step = cpu.step
    br = bisect_right
    try:
        while cpu.icount < budget:
            pc = cpu.pc
            i = br(starts, pc) - 1
            if i >= 0 and pc < ends[i]:
                counts[i] += 1
            else:
                counts[unknown] += 1
            nm = entry_of.get(pc)
            if nm is not None:
                regs = cpu.regs
                d, s, ln = INSTRUMENTED[nm]
                c = calls[nm]
                c["calls"] += 1
                n = regs[ln] if ln is not None else 0
                c["bytes"] += n
                if s is not None and ((regs[d] ^ regs[s]) & 3) == 0:
                    c["aligned_calls"] += 1
                    c["aligned_bytes"] += n
                elif s is None and (regs[d] & 3) == 0:
                    c["aligned_calls"] += 1
                    c["aligned_bytes"] += n
                bucket = "0" if n == 0 else str(1 << (n.bit_length() - 1))
                c["len_hist"][bucket] = c["len_hist"].get(bucket, 0) + 1
            step()
            if len(fc) != len(snapshots):
                snapshot(f"frame_{len(fc) - 1}")
                if len(fc) >= target_frames:
                    break
            if cpu.icount >= next_tick:
                next_tick += 5_000_000
                print(
                    f"#   ... icount={cpu.icount:,} frames={len(fc)} "
                    f"({cpu.icount / max(time.time() - t0, 1e-9) / 1000:.0f} kips)",
                    file=sys.stderr,
                )
    except Halted as h:
        halted = h
        snapshot("halt")
    # --------------------------------------------------------------------

    if not snapshots:
        snapshot("end")
    total = cpu.icount
    print(f"# outcome  : {'HALT ' + halted.reason if halted else 'frame budget reached'}")
    print(f"# icount   : {total:,}   frame_commits: {len(fc)}")
    if fc:
        print(f"# frame commit icounts: {[c for _, c in fc]}")

    result = {
        "rom_sha256": rom_sha,
        "pinned_hash": pinned,
        "pinned_match": rom_sha == pinned,
        "icount": total,
        "frame_commits": [{"frame": f, "icount": c} for f, c in fc],
        "halt": (
            {"reason": halted.reason, "pc": halted.pc, "exit_code": halted.exit_code}
            if halted
            else None
        ),
        "windows": [],
    }

    # Report windows: whole run, boot (0 -> first commit), and steady state
    # (first commit -> last commit), which is the one that represents an
    # actual timedemo. crt0's BSS loop lives entirely inside the boot window
    # and is named explicitly there so it can be subtracted.
    def window(label, lo_counts, hi_counts, lo_calls, hi_calls, icount):
        delta = [hi - lo for hi, lo in zip(hi_counts, lo_counts)]
        tot = sum(delta)
        rows = sorted(
            ((delta[i], names[i] if i < nsym else "<no symbol>") for i in range(nsym + 1)),
            key=lambda r: (-r[0], r[1]),
        )
        cd = {}
        for nm in INSTRUMENTED:
            cd[nm] = {
                k: (hi_calls[nm][k] - lo_calls[nm][k])
                for k in ("calls", "bytes", "aligned_calls", "aligned_bytes")
            }
            cd[nm]["insns"] = delta[names.index(nm)] if nm in names else 0
        w = {
            "label": label,
            "icount": icount,
            "total_attributed": tot,
            "top": [{"fn": n, "insns": c, "pct": 100.0 * c / tot if tot else 0.0} for c, n in rows[:40]],
            "memfns": cd,
        }
        result["windows"].append(w)

        print()
        print(f"=== window: {label}  ({tot:,} instructions) ===")
        for c, n in rows[:30]:
            if c == 0:
                break
            print(f"  {100.0 * c / tot if tot else 0:6.2f}%  {c:>12,}  {n}")
        print("  -- mem/str functions --")
        for nm in ("memcpy", "memmove", "memset", "memcmp", "strlen", "strcpy", "strcmp", "strncpy"):
            d = cd[nm]
            if d["calls"] == 0 and d["insns"] == 0:
                continue
            ipb = d["insns"] / d["bytes"] if d["bytes"] else float("nan")
            print(
                f"  {nm:<8s} insns={d['insns']:>12,} ({100.0 * d['insns'] / tot if tot else 0:5.2f}%) "
                f"calls={d['calls']:>10,} bytes={d['bytes']:>14,} "
                f"insn/byte={ipb:6.3f} "
                f"word-aligned-calls={100.0 * d['aligned_calls'] / d['calls'] if d['calls'] else 0:5.1f}%"
            )

    zero_counts = [0] * (nsym + 1)
    zero_calls = {nm: {"calls": 0, "bytes": 0, "aligned_calls": 0, "aligned_bytes": 0} for nm in INSTRUMENTED}

    first = snapshots[0]
    last = snapshots[-1]
    window("whole run (includes crt0 BSS zeroing)", zero_counts, last[2], zero_calls, last[3], last[1])
    window("boot: 0 -> first FRAME_COMMIT", zero_counts, first[2], zero_calls, first[3], first[1])
    if len(snapshots) > 1:
        window(
            f"steady state: frame 0 -> frame {len(snapshots) - 1} (excludes crt0/boot)",
            first[2],
            last[2],
            first[3],
            last[3],
            last[1] - first[1],
        )

    for spec in args.extra_window:
        lo_s, hi_s = spec.split(":")
        lo, hi = int(lo_s), int(hi_s)
        if not (0 <= lo < hi < len(snapshots)):
            print(f"# skipping --extra-window {spec}: out of range (have {len(snapshots)} snapshots)")
            continue
        window(
            f"frames {lo} -> {hi}",
            snapshots[lo][2],
            snapshots[hi][2],
            snapshots[lo][3],
            snapshots[hi][3],
            snapshots[hi][1] - snapshots[lo][1],
        )

    result["len_hist"] = {nm: calls[nm]["len_hist"] for nm in INSTRUMENTED}
    print()
    print("=== memcpy/memset call-size histogram (whole run, power-of-two buckets) ===")
    for nm in ("memcpy", "memset", "memmove"):
        h = calls[nm]["len_hist"]
        if not h:
            continue
        ordered = sorted(h.items(), key=lambda kv: int(kv[0]))
        print(f"  {nm}: " + "  ".join(f"{k}:{v:,}" for k, v in ordered))

    if args.json:
        Path(args.json).write_text(json.dumps(result, indent=2))
        print(f"\n# json written to {args.json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
