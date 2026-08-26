"""SPEC §7 checkpoint trace emitter -- the differential contract. Both
refemu and sqlcpu must emit byte-identical trace files, so every detail
SPEC.md leaves unstated here is decided in this file and must be matched
exactly on the sqlcpu side (issue #15 / #22 -- coordinate before either
side merges, per CLAUDE.md).

Decisions, and why:

- **xxh64 seed: 0.** Matches ClickHouse's `xxHash64(x)` called with no
  explicit seed. Empirically verified, not assumed: `xxhash.xxh64(buf,
  seed=0).intdigest()` in Python and `SELECT xxHash64(<same bytes>)` in
  ClickHouse 26.3 (the repo pin) both produced `2458886301344574297` for
  the same 128-byte input.

- **Hex fields: lowercase, zero-padded, no `0x` prefix.** `pc_hex` is 8
  digits (32 bits); `reghash_hex`/`ramhash_hex` are 16 digits (64 bits).
  ClickHouse's `hex()` is *uppercase and not zero-padded* by default
  (`hex(toUInt64(255))` → `"FF"`, not `"00000000000000ff"`) -- sqlcpu
  needs `lpad(lower(hex(x)), 16, '0')` (or 8, for pc) to match. This was
  the single easiest place for the two engines to silently diverge, so
  it's spelled out here rather than left to "obviously" match.

- **reghash bytes: `pc || regs[1..31]`, each a 4-byte little-endian word,
  concatenated in register-index order.** x0 is never hashed -- it's
  always 0 by construction (SPEC §1), so hashing it would only add
  constant bytes to every checkpoint for no signal.

- **ramhash bytes: the full RAM region (SPEC §2's 24 MiB RAM, not MMIO /
  FRAMEBUFFER / PALETTE), address-ascending, each word already stored
  little-endian.** refemu's `Memory.ram` bytearray is already exactly
  this byte sequence -- no re-serialization needed, which is also a
  reason to keep it that way rather than "optimizing" the in-memory
  layout later without updating this comment.

- **fbhash: a separate column, not folded into ramhash.** Settled in
  issue #55/#56 after refemu and sqlcpu independently proposed the same
  answer before comparing notes: SPEC §7's ram_hash covers only the RAM
  region, leaving FRAMEBUFFER and PALETTE (SPEC §2) unchecked by any
  checkpoint. A rendering bug that writes the wrong byte to the
  framebuffer perturbs no register and isn't in RAM, so reghash and
  ramhash both stay identical while the actual DOOM output silently
  diverges -- exactly the failure mode SPEC §7 exists to catch early. A
  *separate* column (rather than one hash over all three regions) means a
  divergence hunt learns immediately which region disagreed, without
  having to re-hash subregions after the fact to bisect -- real value
  specifically because Phase 3 is when this needs to be fast, not just
  possible. Bytes: xxh64 over FRAMEBUFFER (64,000 B) `||` PALETTE (768 B),
  address-ascending, concatenated in that order, 64,768 bytes total; same
  seed and hex formatting as every other hash column here. Present only
  on RAM_HASH_INTERVAL lines, same cadence as ramhash.
"""

from __future__ import annotations

from collections.abc import Iterator

import xxhash

from .cpu import CPU, Halted

CHECKPOINT_INTERVAL = 4_096
RAM_HASH_INTERVAL = 1_048_576  # a multiple of CHECKPOINT_INTERVAL (256x)

XXH64_SEED = 0


def reg_hash(pc: int, regs: list[int]) -> int:
    """xxh64 over pc || regs[1..31] as described in this module's
    docstring. Returns the raw unsigned 64-bit digest (format with
    `format_checkpoint`, not this function, to keep hashing and text
    formatting separately testable)."""
    buf = pc.to_bytes(4, "little") + b"".join(r.to_bytes(4, "little") for r in regs[1:32])
    return xxhash.xxh64(buf, seed=XXH64_SEED).intdigest()


def ram_hash(ram: bytes | bytearray) -> int:
    """xxh64 over the full RAM region, already little-endian word bytes
    in address order (see module docstring)."""
    return xxhash.xxh64(bytes(ram), seed=XXH64_SEED).intdigest()


def fb_hash(framebuffer: bytes | bytearray, palette: bytes | bytearray) -> int:
    """xxh64 over FRAMEBUFFER || PALETTE (issue #55/#56), each already
    stored address-ascending -- same no-re-serialization property as
    `ram_hash`. MMIO is deliberately excluded: it's live device state,
    not a value two independently-running engines should agree on
    bit-for-bit."""
    return xxhash.xxh64(bytes(framebuffer) + bytes(palette), seed=XXH64_SEED).intdigest()


def format_checkpoint(
    icount: int,
    pc: int,
    reghash: int,
    ramhash: int | None = None,
    fbhash: int | None = None,
) -> str:
    """One SPEC §7 TSV line:
    icount<TAB>pc_hex<TAB>reghash_hex[<TAB>ramhash_hex<TAB>fbhash_hex].
    `fbhash` is only meaningful (and only ever passed) alongside `ramhash`
    -- both are RAM_HASH_INTERVAL-cadence columns, per issue #55/#56."""
    fields = [str(icount), f"{pc:08x}", f"{reghash:016x}"]
    if ramhash is not None:
        fields.append(f"{ramhash:016x}")
    if fbhash is not None:
        fields.append(f"{fbhash:016x}")
    return "\t".join(fields)


def run_trace(cpu: CPU, max_instructions: int) -> tuple[list[str], Halted | None]:
    """Run `cpu` for up to `max_instructions`, collecting one formatted
    SPEC §7 checkpoint line every CHECKPOINT_INTERVAL retired
    instructions (with a ram_hash appended every RAM_HASH_INTERVAL).
    Stops early on halt -- a halt mid-trace is a normal outcome (the
    program finished or faulted), not an error, so it's returned rather
    than raised. Returns `(lines, halt)`; `halt` is `None` only if
    `max_instructions` was reached without the CPU halting.
    """
    lines: list[str] = []
    halt: Halted | None = None
    while cpu.icount < max_instructions:
        try:
            cpu.step()
        except Halted as h:
            halt = h
            break
        if cpu.icount % CHECKPOINT_INTERVAL == 0:
            rh = reg_hash(cpu.pc, cpu.regs)
            at_ram_interval = cpu.icount % RAM_HASH_INTERVAL == 0
            ramh = ram_hash(cpu.memory.ram) if at_ram_interval else None
            fbh = fb_hash(cpu.memory.framebuffer, cpu.memory.palette) if at_ram_interval else None
            lines.append(format_checkpoint(cpu.icount, cpu.pc, rh, ramh, fbh))
    return lines, halt


def iter_trace(cpu: CPU, max_instructions: int) -> Iterator[str]:
    """Streaming form of `run_trace`, for callers writing a large trace to
    a file without holding it all in memory (e.g. a future `-timedemo`
    run). Propagates `Halted` instead of catching it -- the caller decides
    what a halt mid-stream means for whatever it's writing to."""
    while cpu.icount < max_instructions:
        cpu.step()
        if cpu.icount % CHECKPOINT_INTERVAL == 0:
            rh = reg_hash(cpu.pc, cpu.regs)
            at_ram_interval = cpu.icount % RAM_HASH_INTERVAL == 0
            ramh = ram_hash(cpu.memory.ram) if at_ram_interval else None
            fbh = fb_hash(cpu.memory.framebuffer, cpu.memory.palette) if at_ram_interval else None
            yield format_checkpoint(cpu.icount, cpu.pc, rh, ramh, fbh)


def _main() -> int:  # pragma: no cover -- thin argument-parsing shell
    """`python -m refemu <image.bin> [--max-instructions N]` (see
    `__main__.py`, which is what actually invokes this): load a flat
    binary at SPEC §2's RAM base and print its SPEC §7 trace to stdout,
    TSV, one checkpoint per line -- the interface `scripts/diff_run.sh`
    (executor workstream) runs against refemu's side of a differential
    comparison. Halt info (if any) goes to stderr, not stdout, so stdout
    stays pure trace data.
    """
    import argparse
    import sys

    from .memory import Memory

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", help="flat binary, loaded verbatim at RAM_BASE (SPEC §4)")
    parser.add_argument("--max-instructions", type=int, default=10_000_000)
    parser.add_argument("--text-start", type=lambda s: int(s, 0), default=None)
    parser.add_argument("--text-end", type=lambda s: int(s, 0), default=None)
    args = parser.parse_args()

    memory = Memory(text_start=args.text_start, text_end=args.text_end)
    with open(args.image, "rb") as f:
        memory.load_image(f.read())
    cpu = CPU(memory=memory)

    try:
        for line in iter_trace(cpu, args.max_instructions):
            print(line)
    except Halted as h:
        detail = "".join(
            [
                f" insn=0x{h.insn:08x}" if h.insn is not None else "",
                f" addr=0x{h.addr:08x}" if h.addr is not None else "",
                f" exit_code={h.exit_code}" if h.exit_code is not None else "",
            ]
        )
        print(f"# halted: {h.reason} at pc=0x{h.pc:08x} icount={cpu.icount}{detail}", file=sys.stderr)
        return 0

    print(f"# reached --max-instructions ({args.max_instructions}) without halting", file=sys.stderr)
    return 0
