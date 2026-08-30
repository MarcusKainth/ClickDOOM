"""The Python interpreter, driven a batch of cases at a time.

One long-lived process rather than one per case: starting an interpreter
costs about fifty milliseconds, which would cap a differential at twenty
cases a second. Reading a batch per line and answering a batch per line runs
three orders of magnitude faster than that, which is what makes comparing
millions of cases worth building.

Reads one JSON object per line on stdin, each carrying a list of cases.
Writes one JSON array per line on stdout, one entry per case. Exists for the
migration to Rust and is removed with the interpreter it drives.
"""

import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent.parent / "src"))

from refemu import trace as trace_module
from refemu.cpu import Halted, new_cpu


def run_case(case):
    """Run one case and report everything the machine can be asked about."""
    text_start, text_end = (case["text"] or (None, None))
    cpu = new_cpu(
        ram_size=case["ram_size"],
        ipms=case["ipms"],
        text_start=text_start,
        text_end=text_end,
    )
    image = b"".join(w.to_bytes(4, "little") for w in case["words"])
    cpu.memory.load_image(image, base=case["base"])
    cpu.pc = case["pc"]
    for index, value in enumerate(case["regs"]):
        cpu.write_reg(index, value)
    for pressed, doomkey in case["keyq"]:
        cpu.memory.mmio.push_key(bool(pressed), doomkey)

    # The real emitter, at a cadence small enough for a short case to reach.
    # Patching the module's own constants is how its tests reach them too.
    trace_module.CHECKPOINT_INTERVAL = case["checkpoint_interval"]
    trace_module.RAM_HASH_INTERVAL = case["ram_hash_interval"]
    lines, halt = trace_module.run_trace(cpu, case["steps"])

    return {
        "icount": cpu.icount,
        "pc": cpu.pc,
        "regs": list(cpu.regs),
        "halt": None
        if halt is None
        else {
            "reason": halt.reason,
            "pc": halt.pc,
            "insn": halt.insn,
            "addr": halt.addr,
            "exit_code": halt.exit_code,
        },
        "reghash": trace_module.reg_hash(cpu.pc, cpu.regs),
        "ramhash": trace_module.ram_hash(cpu.memory.ram),
        "fbhash": trace_module.fb_hash(cpu.memory.framebuffer, cpu.memory.palette),
        "console": list(cpu.memory.mmio.console_out),
        "frame_commits": [list(c) for c in cpu.memory.mmio.frame_commits],
        "keyq": [list(k) for k in cpu.memory.mmio.key_queue],
        "checkpoints": lines,
    }


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        batch = json.loads(line)
        out = []
        for case in batch:
            try:
                out.append(run_case(case))
            except Halted as halt:  # pragma: no cover -- run_trace catches these
                out.append({"error": f"escaped Halted: {halt.reason}"})
            except Exception as error:  # noqa: BLE001
                out.append({"error": f"{type(error).__name__}: {error}"})
        sys.stdout.write(json.dumps(out) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
