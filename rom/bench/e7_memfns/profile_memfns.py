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
import os
import subprocess
import tempfile
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROM_DIR = HERE.parent.parent  # rom/
REPO = ROM_DIR.parent
sys.path.insert(0, str(HERE))

from elfsyms import read_func_symbols  # noqa: E402


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


REFEMU = os.environ.get("REFEMU", "./target/release/refemu")


def read_rows(path: Path, columns: tuple[str, ...]) -> list[dict[str, int]]:
    """Every non-comment row of a report, as ints keyed by column name."""
    rows = []
    for line in path.read_text().splitlines():
        if line.startswith("#") or not line:
            continue
        rows.append(dict(zip(columns, (int(v) for v in line.split("\t")), strict=True)))
    return rows


def read_histogram(path: Path) -> list[tuple[str, list[tuple[int, int]], int]]:
    """Each snapshot as (label, [(pc, count)] ascending by pc, retired)."""
    retired_of: dict[str, int] = {}
    rows: dict[str, list[tuple[int, int]]] = {}
    order: list[str] = []
    for line in path.read_text().splitlines():
        if line.startswith("# snapshot"):
            _, label, retired, _distinct, _counted = line.split("\t")
            retired_of[label] = int(retired.split("=")[1])
            order.append(label)
            rows.setdefault(label, [])
        elif not line.startswith("#") and line:
            label, pc_hex, count = line.split("\t")
            rows[label].append((int(pc_hex, 16), int(count)))
    return [(label, rows[label], retired_of[label]) for label in order]


def read_traps(path: Path) -> list[tuple[int, str, dict[int, int]]]:
    """Each call as (count before it, name, register number to value)."""
    out = []
    registers: list[int] = []
    for line in path.read_text().splitlines():
        if line.startswith("# columns"):
            registers = [int(name[1:]) for name in line.split("\t")[4:]]
        elif not line.startswith("#") and line:
            fields = line.split("\t")
            out.append(
                (
                    int(fields[0]),
                    fields[2],
                    dict(zip(registers, (int(v) for v in fields[3:]), strict=True)),
                )
            )
    return out


def build_symbol_table(elf_path: Path):
    syms = read_func_symbols(elf_path)
    # Two symbols can share an address. Attribution walks the histogram in
    # address order and takes the first match, so collapse each equal-address
    # run to one entry rather than leaving which of them wins to the order the
    # ELF happens to list them in.
    collapsed = []
    for sym in syms:
        if collapsed and collapsed[-1][0] == sym[0]:
            collapsed[-1] = sym
        else:
            collapsed.append(sym)
    syms = collapsed
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

    starts, ends, names = build_symbol_table(elf_path)
    nsym = len(names)
    unknown = nsym  # the slot for a program counter in no known symbol
    print(f"# symbols  : {nsym} STT_FUNC")

    entry_of = {}
    for i, nm in enumerate(names):
        if nm in INSTRUMENTED and nm not in entry_of:
            entry_of[starts[i]] = nm
    print(f"# instrumented entries: {sorted(entry_of.values())}")

    # Which frames the report needs a snapshot at. The run announces many
    # more; asking for a snapshot at every one would copy the whole histogram
    # each time for windows nobody reports.
    target_frames = max(args.frames, 1)
    wanted = {0, target_frames - 1}
    for spec in args.extra_window:
        lo, hi = (int(v) for v in spec.split(":"))
        wanted.update((lo, hi))
    points = [f"frame:{n}" for n in sorted(wanted)]

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        trap_file = tmp / "traps.txt"
        trap_file.write_text(
            "".join(f"{addr:08x}\t{name}\n" for addr, name in sorted(entry_of.items()))
        )
        hist_file, trap_report, frame_log, halt_report = (
            tmp / "hist.tsv",
            tmp / "traps.tsv",
            tmp / "frames.tsv",
            tmp / "halt.json",
        )
        command = [
            REFEMU, "run", str(rom_path),
            "--manifest", str(args.manifest),
            "--stop-at", f"frame:{target_frames - 1}",
            "--stop-at", "halt",
            "--stop-at", "budget",
            "--max-instructions", str(args.max_instructions),
            "--pc-histogram", str(hist_file),
            "--trap-pcs", str(trap_file),
            "--trap-report", str(trap_report),
            "--frame-log", str(frame_log),
            "--halt-report", str(halt_report),
        ]
        for point in points:
            command += ["--histogram-at", point]
        result = subprocess.run(command, check=False)  # purity-ok: measurement tooling; refemu does the emulation and this reads its reports
        if result.returncode != 0:
            print(f"::error::refemu exited {result.returncode}", file=sys.stderr)
            return 1

        histogram = read_histogram(hist_file)
        traps = read_traps(trap_report)
        fc = [
            (row["frame_no"], row["commit_icount"])
            for row in read_rows(frame_log, ("index", "frame_no", "commit_icount", "retired_icount"))
        ]
        report = json.loads(halt_report.read_text())

    total = report["icount"]
    halted = report["halt"]

    # Attribution: the histogram rows are in address order and the symbol
    # ranges are sorted and non-overlapping, so this is a linear merge.
    def attribute(rows):
        out = [0] * (nsym + 1)
        index = 0
        for pc, count in rows:
            while index < nsym and ends[index] <= pc:
                index += 1
            if index < nsym and starts[index] <= pc:
                out[index] += count
            else:
                out[unknown] += count
        return out

    def call_stats(lo_icount, hi_icount):
        out = {
            nm: {"calls": 0, "bytes": 0, "aligned_calls": 0, "aligned_bytes": 0, "len_hist": {}}
            for nm in INSTRUMENTED
        }
        for icount, name, regs in traps:
            if not lo_icount <= icount < hi_icount or name not in INSTRUMENTED:
                continue
            d, s, ln = INSTRUMENTED[name]
            c = out[name]
            c["calls"] += 1
            n = regs.get(ln, 0) if ln is not None else 0
            c["bytes"] += n
            aligned = ((regs[d] ^ regs[s]) & 3) == 0 if s is not None else (regs[d] & 3) == 0
            if aligned:
                c["aligned_calls"] += 1
                c["aligned_bytes"] += n
            bucket = "0" if n == 0 else str(1 << (n.bit_length() - 1))
            c["len_hist"][bucket] = c["len_hist"].get(bucket, 0) + 1
        return out

    # Snapshots in the shape the reporting below already expects.
    snapshots = []
    for label, rows, retired in histogram:
        name = "end" if label == "end" else f"frame_{label.split(':')[1]}"
        snapshots.append((name, retired, attribute(rows), call_stats(0, retired)))

    assert snapshots, "the emulator reported no histogram snapshots"
    total = report["icount"]
    print(f"# outcome  : {'HALT ' + halted['reason'] if halted else 'frame budget reached'}")
    print(f"# icount   : {total:,}   frame_commits: {len(fc)}")
    if fc:
        print(f"# frame commit icounts: {[c for _, c in fc]}")

    result = {
        "rom_sha256": rom_sha,
        "pinned_hash": pinned,
        "pinned_match": rom_sha == pinned,
        "icount": total,
        "frame_commits": [{"frame": f, "icount": c} for f, c in fc],
        "halt": halted,
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
            f"steady state: frame 0 -> frame {target_frames - 1} (excludes crt0/boot)",
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

    whole_run_calls = snapshots[-1][3]
    result["len_hist"] = {nm: whole_run_calls[nm]["len_hist"] for nm in INSTRUMENTED}
    print()
    print("=== memcpy/memset call-size histogram (whole run, power-of-two buckets) ===")
    for nm in ("memcpy", "memset", "memmove"):
        h = whole_run_calls[nm]["len_hist"]
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
