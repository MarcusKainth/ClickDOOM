"""Boot report for a ROM image (issue #16, Phase 1's second refemu
milestone: "ROM boots in refemu").

Loads a flat binary per SPEC §4's manifest and runs it, reporting exactly
one of three outcomes: a clean run to the first `FRAME_COMMIT`, a precise
fault report (pc, instruction word, halt reason, and the last 20 retired
pcs), or -- not named in the issue but necessary for a bounded run against
a program that never halts and never commits a frame -- running out of the
instruction budget. Every run also prints an instruction-mix histogram;
`executor` wants this because ADR-0002 collapses opcode arms and the real
dynamic mix decides which arms are worth collapsing next (their own
ablation was against a synthetic Phase-0 profile, not a real one).

**No real `doom-rv32im.bin` exists yet** -- `rom`'s libc shims (#7),
`DG_*` hooks (#8) and embedded WAD (#9) are all still ahead of crt0/the
linker script (#47, merged) and vendored doomgeneric (#44, merged). This
module is built and tested against `rom`'s crt0 + `main_stub` placeholder
instead (the same ELF independently booted during PR #47's review), so
the reporting machinery is proven correct now, against a known-good
"no fault yet" case, rather than first exercised whenever a real image
shows up and something goes wrong.
"""

from __future__ import annotations

import json
import sys
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path

from .cpu import CPU, Halted, new_cpu
from .memory import RAM_BASE

RETIRED_PC_HISTORY = 20


def classify(insn: int) -> str:
    """Identify an instruction word's mnemonic, for the instruction-mix
    histogram only. Deliberately kept separate from `cpu.py`'s dispatch
    rather than hooking into it: `cpu.py` stays the single source of
    truth for *execution* and untouched by a diagnostic-only concern, so
    a bug here can only ever produce a wrong histogram label, never a
    wrong emulated result. Mirrors `cpu._execute`'s opcode/funct3/funct7
    switch structure by necessity (there is no other way to name an
    instruction), but computes nothing.
    """
    opcode = insn & 0x7F
    funct3 = (insn >> 12) & 0x7
    funct7 = (insn >> 25) & 0x7F

    if opcode == 0x37:
        return "lui"
    if opcode == 0x17:
        return "auipc"
    if opcode == 0x6F:
        return "jal"
    if opcode == 0x67:
        return "jalr" if funct3 == 0 else "illegal"
    if opcode == 0x63:
        return {0: "beq", 1: "bne", 4: "blt", 5: "bge", 6: "bltu", 7: "bgeu"}.get(
            funct3, "illegal"
        )
    if opcode == 0x03:
        return {0: "lb", 1: "lh", 2: "lw", 4: "lbu", 5: "lhu"}.get(funct3, "illegal")
    if opcode == 0x23:
        return {0: "sb", 1: "sh", 2: "sw"}.get(funct3, "illegal")
    if opcode == 0x13:
        if funct3 == 0b001:
            return "slli" if funct7 == 0x00 else "illegal"
        if funct3 == 0b101:
            return {0x00: "srli", 0x20: "srai"}.get(funct7, "illegal")
        return {0: "addi", 2: "slti", 3: "sltiu", 4: "xori", 6: "ori", 7: "andi"}.get(
            funct3, "illegal"
        )
    if opcode == 0x33:
        if funct7 == 0x01:
            return {
                0: "mul",
                1: "mulh",
                2: "mulhsu",
                3: "mulhu",
                4: "div",
                5: "divu",
                6: "rem",
                7: "remu",
            }.get(funct3, "illegal")
        names = {
            (0, 0x00): "add",
            (0, 0x20): "sub",
            (1, 0x00): "sll",
            (2, 0x00): "slt",
            (3, 0x00): "sltu",
            (4, 0x00): "xor",
            (5, 0x00): "srl",
            (5, 0x20): "sra",
            (6, 0x00): "or",
            (7, 0x00): "and",
        }
        return names.get((funct3, funct7), "illegal")
    if opcode == 0x0F:
        return "fence"
    if opcode == 0x73:
        if funct3 == 0:
            imm12 = insn >> 20
            return {0: "ecall", 1: "ebreak"}.get(imm12, "illegal")
        return "csr" if funct3 in (1, 2, 3, 5, 6, 7) else "illegal"
    return "illegal"


@dataclass
class BootReport:
    outcome: str  # "frame_commit" | "halt" | "budget_exhausted"
    icount: int
    histogram: dict[str, int] = field(default_factory=dict)
    retired_pcs: list[int] = field(default_factory=list)
    halt_reason: str | None = None
    halt_pc: int | None = None
    halt_insn: int | None = None
    halt_addr: int | None = None
    halt_exit_code: int | None = None
    frame_no: int | None = None


def boot(
    image: bytes,
    load_addr: int = RAM_BASE,
    text_start: int | None = None,
    text_end: int | None = None,
    max_instructions: int = 10_000_000,
) -> BootReport:
    """Load `image` at `load_addr` (SPEC §4: "loaded verbatim") and run it
    with real MMIO semantics wired up (`cpu.new_cpu`), watching for the
    first `FRAME_COMMIT`, a halt, or the instruction budget running out.
    """
    cpu: CPU = new_cpu(text_start=text_start, text_end=text_end)
    cpu.memory.load_image(image, base=load_addr)
    cpu.pc = load_addr

    histogram: dict[str, int] = {}
    retired_pcs: deque[int] = deque(maxlen=RETIRED_PC_HISTORY)

    while cpu.icount < max_instructions:
        pc_before = cpu.pc
        try:
            cpu.step()
        except Halted as h:
            return BootReport(
                outcome="halt",
                icount=cpu.icount,
                histogram=histogram,
                retired_pcs=list(retired_pcs),
                halt_reason=h.reason,
                halt_pc=h.pc,
                halt_insn=h.insn,
                halt_addr=h.addr,
                halt_exit_code=h.exit_code,
            )

        # Safe to re-read here (unlike pre-fetching before step()): step()
        # just proved this exact address fetches cleanly, so this can't
        # raise BadAddr/Misaligned the way an eager pre-fetch could.
        insn = cpu.memory.read(pc_before, 4)
        retired_pcs.append(pc_before)
        mnemonic = classify(insn)
        histogram[mnemonic] = histogram.get(mnemonic, 0) + 1

        if cpu.memory.mmio.frame_commits:
            frame_no, _committed_icount = cpu.memory.mmio.frame_commits[-1]
            return BootReport(
                outcome="frame_commit",
                icount=cpu.icount,
                histogram=histogram,
                retired_pcs=list(retired_pcs),
                frame_no=frame_no,
            )

    return BootReport(
        outcome="budget_exhausted",
        icount=cpu.icount,
        histogram=histogram,
        retired_pcs=list(retired_pcs),
    )


def format_report(report: BootReport) -> str:
    lines: list[str] = []

    if report.outcome == "frame_commit":
        lines.append(f"CLEAN RUN: reached FRAME_COMMIT (frame {report.frame_no}) at icount={report.icount}")
    elif report.outcome == "halt":
        lines.append(f"FAULT: {report.halt_reason} at pc=0x{report.halt_pc:08x} icount={report.icount}")
        if report.halt_insn is not None:
            lines.append(f"  instruction word: 0x{report.halt_insn:08x}")
        if report.halt_addr is not None:
            lines.append(f"  address: 0x{report.halt_addr:08x}")
        if report.halt_exit_code is not None:
            lines.append(f"  exit code: {report.halt_exit_code}")
    else:
        lines.append(f"BUDGET EXHAUSTED: no fault, no FRAME_COMMIT after icount={report.icount}")

    lines.append("")
    lines.append(f"last {len(report.retired_pcs)} retired pcs (oldest first):")
    for pc in report.retired_pcs:
        lines.append(f"  0x{pc:08x}")

    lines.append("")
    total = sum(report.histogram.values())
    lines.append(f"instruction mix ({total} retired):")
    for mnemonic, count in sorted(report.histogram.items(), key=lambda kv: -kv[1]):
        pct = 100.0 * count / total if total else 0.0
        lines.append(f"  {mnemonic:<8s} {pct:6.2f}%  ({count})")

    return "\n".join(lines)


def _main() -> int:  # pragma: no cover -- thin argument-parsing shell
    """`python -m refemu.boot <image.bin> [--manifest manifest.json] [--max-instructions N]`."""
    import argparse

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="flat binary, loaded verbatim (SPEC §4)")
    parser.add_argument(
        "--manifest",
        help="path to manifest.json (default: <image's directory>/manifest.json if present)",
    )
    parser.add_argument("--max-instructions", type=int, default=10_000_000)
    args = parser.parse_args()

    image_path = Path(args.image)
    manifest_path = Path(args.manifest) if args.manifest else image_path.parent / "manifest.json"

    load_addr = RAM_BASE
    text_start = text_end = None
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())
        load_addr = manifest.get("load_addr", RAM_BASE)
        text_start = manifest.get("text_start")
        text_end = manifest.get("text_end")
        print(f"# manifest: {manifest_path}", file=sys.stderr)
    else:
        print(
            f"# no manifest.json at {manifest_path}; using SPEC §1 defaults "
            f"(load_addr=0x{load_addr:08x}, no text-region protection)",
            file=sys.stderr,
        )

    report = boot(
        image_path.read_bytes(),
        load_addr=load_addr,
        text_start=text_start,
        text_end=text_end,
        max_instructions=args.max_instructions,
    )
    print(format_report(report))

    return {"frame_commit": 0, "halt": 1, "budget_exhausted": 2}[report.outcome]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(_main())
