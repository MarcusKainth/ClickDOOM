#!/usr/bin/env python3
"""Run the real DOOM ROM's `-timedemo demo3` to completion, emitting the
SPEC §7 reference trace the Phase 3 victory run diffs against -- issue
#129, the team lead's ask after #111 established the ROM can actually
invoke README's Definition of Victory.

**Prepared, not run.** Building this and producing a wall-clock estimate
was the assignment; starting the real multi-hour-to-multi-day run is an
explicit hold ("I will schedule it when the machine is quiet") and is not
done by this commit. Use `--estimate-only` (see below) to get the
projection without starting anything durable.

## Why this needs its own script, not a bigger `--max-instructions` on
## `gen_reference_trace.py`

Three properties `gen_reference_trace.py` doesn't need and this does:

1. **No known instruction budget.** `demo3`'s real length is *what this
   harness is for measuring* -- the 2.90-3.14 billion range below is an
   estimate (see "Two different tic-count estimates"), not a number to
   pass as `--max-instructions`. This runs until `Halted`, not until a
   budget is exhausted.
2. **Resumability.** A run this long *will* be interrupted -- machine
   reboots, the human owner wants the box back, a bug needs the process
   killed to fix and restart. A harness that starts over from icount 0 on
   every interruption is a harness that never finishes. See "Resume
   design" below.
3. **Progress reporting.** A multi-day background process needs to be
   externally distinguishable from a *stuck* one without attaching a
   debugger. See "Progress reporting" below.

`gen_reference_trace.py`'s bounded, single-shot design is correct for what
it does (a known, short budget to the first `FRAME_COMMIT`) and shouldn't
be complicated to also do this. Both import the same hash functions from
`refemu.trace` and the same `PINNED_HASH`/filename discipline from
`_rom_provenance.py`, so neither script's checkpoints or provenance
handling can drift from the other's.

## Two different tic-count estimates -- report the range, not a number

`demo3` is 2,134 tics (read from the WAD directly). Two independent
instructions/tic measurements exist and disagree:

- ADR-0004: ~1.36M instructions/tic -> **2.90 billion** instructions.
- The E7 profiling subagent (frames 200-299): ~1.47M instructions/tic ->
  **3.14 billion** instructions.

Neither has been validated against an actual full run -- that is exactly
what running this harness would establish, which is why the estimate below
is reported as a range, not resolved to one number by picking a favourite.

## Resume design

Every `RAM_HASH_INTERVAL` boundary (the same cadence `ram_hash`/`fb_hash`
are already computed at, so this piggybacks on work already being done
rather than adding a new expensive full-RAM pass), if at least
`--checkpoint-every-seconds` of wall-clock time has passed since the last
save, the harness snapshots full CPU state (icount, pc, regs, RAM,
framebuffer, palette, MMIO console/key-queue/frame-commit state) to
`<out>.state.pkl`, written to a temp file and atomically `os.replace()`d
into place -- a crash mid-write can never leave a corrupt or half-written
state file, only the previous good one or a discarded temp file. The
`.tsv` trace file's byte length *at the moment of that snapshot* is stored
in the same state, so on resume the `.tsv` is truncated back to exactly
that offset before continuing -- this is what keeps the trace and the
resumed CPU state from ever disagreeing about what's already been
committed, the same "no half-applied state, ever" property SPEC §6
requires of the executor's own batch commit, applied here to a
single-process resumable run instead of a distributed one.

Checkpointing every `RAM_HASH_INTERVAL` unconditionally (rather than
gating on wall-clock time too) would mean writing a ~24 MiB RAM dump to
disk roughly once a second at this engine's measured throughput --
sustained over a multi-day run, that is real disk wear and I/O contention
for a very small reduction in lost work on a crash. The wall-clock gate
(default 10 minutes) bounds that cost while still keeping "how much
compute might a crash lose" to a number worth stating plainly: at most
`--checkpoint-every-seconds` of wall-clock compute, never more.

## Progress reporting

Every `--progress-every-seconds` (default 30s), writes `<out>.progress.
json` (icount, recent and overall instructions/sec, wall-clock elapsed,
an ETA range computed from the *recent* rate against both ends of the
tic-count estimate above, and a timestamp) and prints the same to stderr.
The timestamp is what makes "stuck" externally distinguishable from
"slow": a progress file that stops updating means the process died or
hung, not that DOOM's own code got slower.

## SIGINT/SIGTERM: clean stop, not a crash

Both are caught to set a flag checked once per `CHECKPOINT_INTERVAL`
(cheap) rather than acted on inside the handler itself (signal handlers
running in the middle of a state save would corrupt it) -- the loop
finishes its current step, saves state and the progress file one last
time, and exits 0. An operator-requested stop should never cost more
progress than an actual crash would.

## What gets committed: the manifest, not the trace

The completed `.tsv` (~700K-766K lines across the estimated instruction
range, ~20-30 MB) is **not** meant to be committed -- derived data that
changes with every ROM doesn't belong in a source repo, and regenerating
it is a ~50-minute `just` recipe away, not a multi-day job. What's durable
is the *answer*, not the bytes: on halt, this script writes `<out>.json`
(alongside the `.tsv`, git-ignored) containing the final icount, the
final checkpoint line (`fb_hash` included), the halt reason and
`exit_code`, the `PINNED_HASH` it ran against, and the `.tsv` file's own
sha256 -- so regeneration is independently verifiable (rebuild, compare
the manifest) without the trace bytes themselves living in git. Only the
manifest is intended to be committed once a real run completes.

Contrast with `gen_reference_trace.py`'s (#114) boot-to-first-frame trace,
which **is** committed in full: that one is 3,328 lines / 112 KB, small
enough that the convenience of having it in the repo outright outweighs
the derived-data argument. Different sizes crossing a considered
threshold, not an inconsistency between the two scripts.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pickle
import signal
import sys
import time
from collections import deque
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from refemu.cpu import CPU, Halted, new_cpu
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

REPO_ROOT = Path(__file__).resolve().parent.parent.parent

# demo3's tic count is read directly from the WAD; both instructions/tic
# figures are measurements this harness exists to replace with a real
# one -- see module docstring's "Two different tic-count estimates".
DEMO3_TICS = 2_134
LOW_INSTR_PER_TIC = 1_360_000  # ADR-0004
HIGH_INSTR_PER_TIC = 1_470_000  # E7 profiling subagent, frames 200-299
LOW_ESTIMATE_INSTRUCTIONS = DEMO3_TICS * LOW_INSTR_PER_TIC
HIGH_ESTIMATE_INSTRUCTIONS = DEMO3_TICS * HIGH_INSTR_PER_TIC

# Issue #121: the expected (not yet SPEC-pinned) exit code for a
# successful -timedemo completion. Reported, not asserted -- #121 is
# still an open question for the human owner, so a run producing a
# different exit_code is evidence for that issue, not a bug in this
# script.
EXPECTED_VICTORY_EXIT_CODE = 4_294_967_295


class StopRequested(Exception):
    """Raised from the main loop once a caught SIGINT/SIGTERM's flag is
    observed, so state-save-and-exit happens in one place regardless of
    which check noticed it."""


def _human_seconds(seconds: float) -> str:
    days, rem = divmod(seconds, 86400)
    hours, rem = divmod(rem, 3600)
    minutes, _ = divmod(rem, 60)
    parts = []
    if days:
        parts.append(f"{int(days)}d")
    if hours or days:
        parts.append(f"{int(hours)}h")
    parts.append(f"{int(minutes)}m")
    return " ".join(parts)


def estimate_wall_clock(instructions_per_second: float) -> dict:
    low_s = LOW_ESTIMATE_INSTRUCTIONS / instructions_per_second
    high_s = HIGH_ESTIMATE_INSTRUCTIONS / instructions_per_second
    return {
        "instructions_per_second": instructions_per_second,
        "low_estimate_instructions": LOW_ESTIMATE_INSTRUCTIONS,
        "high_estimate_instructions": HIGH_ESTIMATE_INSTRUCTIONS,
        "low_estimate_seconds": low_s,
        "high_estimate_seconds": high_s,
        "low_estimate_human": _human_seconds(low_s),
        "high_estimate_human": _human_seconds(high_s),
    }


def save_state(path: Path, cpu: CPU, tsv_byte_offset: int, elapsed_seconds: float) -> None:
    """Atomic: write to a temp file in the same directory, then
    `os.replace()` -- a crash mid-write leaves either the previous good
    state file or a discarded `.tmp`, never a half-written state file."""
    state = {
        "icount": cpu.icount,
        "pc": cpu.pc,
        "regs": list(cpu.regs),
        "ram": bytes(cpu.memory.ram),
        "framebuffer": bytes(cpu.memory.framebuffer),
        "palette": bytes(cpu.memory.palette),
        "console_out": bytes(cpu.memory.mmio.console_out),
        "key_queue": list(cpu.memory.mmio.key_queue),
        "frame_commits": list(cpu.memory.mmio.frame_commits),
        "ipms": cpu.memory.mmio.ipms,
        "tsv_byte_offset": tsv_byte_offset,
        "elapsed_seconds": elapsed_seconds,
    }
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    with open(tmp_path, "wb") as f:
        pickle.dump(state, f, protocol=pickle.HIGHEST_PROTOCOL)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp_path, path)


def load_state(path: Path) -> dict:
    with open(path, "rb") as f:
        return pickle.load(f)


def cpu_from_state(state: dict, text_start: int | None, text_end: int | None) -> CPU:
    """Reconstructs a CPU exactly as `save_state` captured it -- does
    NOT reload the pristine ROM image (unlike a fresh start): RAM has
    since diverged from the image by definition, and the saved `ram`
    bytes ARE the current, correct RAM contents."""
    cpu = new_cpu(ipms=state["ipms"], text_start=text_start, text_end=text_end)
    cpu.icount = state["icount"]
    cpu.pc = state["pc"]
    cpu.regs = list(state["regs"])
    cpu.memory.ram[:] = state["ram"]
    cpu.memory.framebuffer[:] = state["framebuffer"]
    cpu.memory.palette[:] = state["palette"]
    cpu.memory.mmio.console_out = bytearray(state["console_out"])
    cpu.memory.mmio.key_queue = deque(state["key_queue"])
    cpu.memory.mmio.frame_commits = list(state["frame_commits"])
    return cpu


def write_progress(path: Path, progress: dict) -> None:
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    tmp_path.write_text(json.dumps(progress, indent=2) + "\n")
    os.replace(tmp_path, path)


def run(
    cpu: CPU,
    tsv_path: Path,
    state_path: Path,
    progress_path: Path,
    max_instructions: int,
    checkpoint_every_seconds: float,
    progress_every_seconds: float,
    resumed_elapsed_seconds: float,
    resumed_tsv_offset: int,
) -> dict:
    """The shared loop: periodic §7 checkpoints appended to `tsv_path`,
    periodic resumable state saves, periodic progress reports. Runs until
    `Halted`, `StopRequested` (SIGINT/SIGTERM), or `max_instructions` is
    reached (a safety cap for tests/estimation, not expected to bind on a
    real run). Returns a summary dict; does not raise `Halted` -- a halt
    mid-run is the normal, hoped-for outcome (Phase 3's finish line), not
    an error.
    """
    stop_requested = False

    def _request_stop(signum, _frame):
        nonlocal stop_requested
        stop_requested = True

    old_sigint = signal.signal(signal.SIGINT, _request_stop)
    old_sigterm = signal.signal(signal.SIGTERM, _request_stop)

    tsv_path.parent.mkdir(parents=True, exist_ok=True)
    tsv_bytes_written = resumed_tsv_offset

    run_start_wall = time.monotonic()
    last_state_save_wall = run_start_wall
    last_progress_wall = run_start_wall
    last_progress_icount = cpu.icount
    halt_info = None
    stopped_early = False

    try:
        with open(tsv_path, "a+b") as tsv_file:
            tsv_file.seek(resumed_tsv_offset)
            tsv_file.truncate(resumed_tsv_offset)

            try:
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

                    if cpu.icount % CHECKPOINT_INTERVAL == 0:
                        rh = reg_hash(cpu.pc, cpu.regs)
                        at_ram_interval = cpu.icount % RAM_HASH_INTERVAL == 0
                        ramh = ram_hash(cpu.memory.ram) if at_ram_interval else None
                        fbh = (
                            fb_hash(cpu.memory.framebuffer, cpu.memory.palette) if at_ram_interval else None
                        )
                        line = format_checkpoint(cpu.icount, cpu.pc, rh, ramh, fbh) + "\n"
                        tsv_file.write(line.encode())
                        tsv_bytes_written += len(line.encode())

                        if at_ram_interval:
                            now = time.monotonic()
                            if now - last_state_save_wall >= checkpoint_every_seconds:
                                tsv_file.flush()
                                os.fsync(tsv_file.fileno())
                                elapsed = resumed_elapsed_seconds + (now - run_start_wall)
                                save_state(state_path, cpu, tsv_bytes_written, elapsed)
                                last_state_save_wall = now

                        now = time.monotonic()
                        if now - last_progress_wall >= progress_every_seconds:
                            recent_instr = cpu.icount - last_progress_icount
                            recent_rate = (
                                recent_instr / (now - last_progress_wall) if now > last_progress_wall else 0
                            )
                            elapsed = resumed_elapsed_seconds + (now - run_start_wall)
                            overall_rate = cpu.icount / elapsed if elapsed > 0 else 0
                            est = estimate_wall_clock(recent_rate) if recent_rate > 0 else None
                            progress = {
                                "icount": cpu.icount,
                                "elapsed_seconds": round(elapsed, 1),
                                "recent_instructions_per_second": round(recent_rate),
                                "overall_instructions_per_second": round(overall_rate),
                                "remaining_seconds_low": (
                                    round(est["low_estimate_seconds"] - elapsed) if est else None
                                ),
                                "remaining_seconds_high": (
                                    round(est["high_estimate_seconds"] - elapsed) if est else None
                                ),
                                "last_updated_unix": time.time(),
                            }
                            write_progress(progress_path, progress)
                            print(
                                f"# icount={cpu.icount} elapsed={_human_seconds(elapsed)} "
                                f"recent={round(recent_rate)} instr/sec overall={round(overall_rate)} instr/sec",
                                file=sys.stderr,
                            )
                            last_progress_wall = now
                            last_progress_icount = cpu.icount

                    if stop_requested:
                        raise StopRequested()
            finally:
                # Runs on every way out of the loop -- halt, StopRequested,
                # or max_instructions exhausted -- so the .tsv is always
                # durable on disk before save_state() below records the
                # byte offset a resume will trust.
                tsv_file.flush()
                os.fsync(tsv_file.fileno())
    except StopRequested:
        stopped_early = True
    finally:
        signal.signal(signal.SIGINT, old_sigint)
        signal.signal(signal.SIGTERM, old_sigterm)

    final_elapsed = resumed_elapsed_seconds + (time.monotonic() - run_start_wall)
    # Save state on any stopping condition (halt, SIGINT/TERM, or hitting
    # max_instructions) so a subsequent resume never has to redo more
    # than the wall-clock gate above already allowed -- not just on the
    # periodic cadence during normal running.
    save_state(state_path, cpu, tsv_bytes_written, final_elapsed)

    return {
        "final_icount": cpu.icount,
        "halt": halt_info,
        "stopped_early": stopped_early,
        "elapsed_seconds": final_elapsed,
        "tsv_bytes_written": tsv_bytes_written,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--image", default=str(REPO_ROOT / "rom" / "build" / "doom-rv32im.bin"))
    parser.add_argument("--manifest", default=str(REPO_ROOT / "rom" / "build" / "manifest.json"))
    parser.add_argument("--pinned-hash", default=str(REPO_ROOT / "rom" / "PINNED_HASH"))
    parser.add_argument(
        "--out-dir", default=str(REPO_ROOT / "refemu" / "reference_traces" / "demo3"), help="directory for output/state/progress files"
    )
    parser.add_argument("--checkpoint-every-seconds", type=float, default=600.0)
    parser.add_argument("--progress-every-seconds", type=float, default=30.0)
    parser.add_argument(
        "--max-instructions",
        type=int,
        default=None,
        help="safety cap; default none (run until Halted). Set low for testing.",
    )
    parser.add_argument(
        "--fresh", action="store_true", help="ignore any existing resume state and start over (destructive)"
    )
    parser.add_argument(
        "--estimate-only",
        type=int,
        metavar="N",
        default=None,
        help=(
            "run N instructions (through the same checkpoint/progress loop, so the"
            " measured rate includes real overhead) and print a wall-clock estimate"
            " for the full run instead of doing anything resumable/durable. Does not"
            " write to --out-dir."
        ),
    )
    args = parser.parse_args()

    image_path = Path(args.image)
    manifest_path = Path(args.manifest)
    pinned_hash_path = Path(args.pinned_hash)
    image = image_path.read_bytes()
    manifest = json.loads(manifest_path.read_text())

    try:
        rom_sha256 = assert_pinned_hash(image, pinned_hash_path)
    except UnpinnedRomError as e:
        print(f"FATAL: {e}", file=sys.stderr)
        return 1
    print(f"# ROM sha256 matches PINNED_HASH: {rom_sha256}", file=sys.stderr)

    if args.estimate_only is not None:
        import tempfile

        cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
        cpu.memory.load_image(image, base=RAM_BASE)
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            t0 = time.monotonic()
            summary = run(
                cpu,
                tsv_path=tmp_path / "sample.tsv",
                state_path=tmp_path / "sample.state.pkl",
                progress_path=tmp_path / "sample.progress.json",
                max_instructions=args.estimate_only,
                checkpoint_every_seconds=1e18,  # never, this is a throwaway sample
                progress_every_seconds=1e18,
                resumed_elapsed_seconds=0.0,
                resumed_tsv_offset=0,
            )
            elapsed = time.monotonic() - t0
        rate = summary["final_icount"] / elapsed if elapsed > 0 else 0
        est = estimate_wall_clock(rate)
        print(f"# sampled {summary['final_icount']} instructions in {elapsed:.2f}s = {rate:.0f} instr/sec", file=sys.stderr)
        print(
            f"# demo3 estimate: {est['low_estimate_human']} - {est['high_estimate_human']} "
            f"({est['low_estimate_seconds']:.0f}s - {est['high_estimate_seconds']:.0f}s, "
            f"{LOW_ESTIMATE_INSTRUCTIONS:,} - {HIGH_ESTIMATE_INSTRUCTIONS:,} instructions)",
            file=sys.stderr,
        )
        print(json.dumps(est, indent=2))
        return 0

    out_dir = Path(args.out_dir)
    tsv_path = out_dir / hashed_filename("demo3", rom_sha256, ".tsv")
    state_path = out_dir / hashed_filename("demo3", rom_sha256, ".state.pkl")
    progress_path = out_dir / hashed_filename("demo3", rom_sha256, ".progress.json")
    meta_path = out_dir / hashed_filename("demo3", rom_sha256, ".json")

    resumed_elapsed = 0.0
    resumed_tsv_offset = 0
    if state_path.exists() and not args.fresh:
        print(f"# resuming from {state_path}", file=sys.stderr)
        state = load_state(state_path)
        cpu = cpu_from_state(state, manifest.get("text_start"), manifest.get("text_end"))
        resumed_elapsed = state["elapsed_seconds"]
        resumed_tsv_offset = state["tsv_byte_offset"]
        print(f"# resumed at icount={cpu.icount}, tsv offset={resumed_tsv_offset}", file=sys.stderr)
    else:
        if args.fresh and state_path.exists():
            print(f"# --fresh: ignoring existing {state_path}", file=sys.stderr)
        cpu = new_cpu(text_start=manifest.get("text_start"), text_end=manifest.get("text_end"))
        cpu.memory.load_image(image, base=RAM_BASE)

    max_instructions = args.max_instructions if args.max_instructions is not None else 2**63

    summary = run(
        cpu,
        tsv_path=tsv_path,
        state_path=state_path,
        progress_path=progress_path,
        max_instructions=max_instructions,
        checkpoint_every_seconds=args.checkpoint_every_seconds,
        progress_every_seconds=args.progress_every_seconds,
        resumed_elapsed_seconds=resumed_elapsed,
        resumed_tsv_offset=resumed_tsv_offset,
    )

    print(
        f"# stopped: icount={summary['final_icount']} halt={summary['halt']} "
        f"stopped_early={summary['stopped_early']} elapsed={_human_seconds(summary['elapsed_seconds'])}",
        file=sys.stderr,
    )

    if summary["halt"] is not None:
        h = summary["halt"]
        if h["reason"] == "EXIT":
            if h["exit_code"] == EXPECTED_VICTORY_EXIT_CODE:
                print(f"# clean EXIT, exit_code={h['exit_code']} matches issue #121's expected value", file=sys.stderr)
            else:
                print(
                    f"# EXIT, but exit_code={h['exit_code']} does NOT match issue #121's expected "
                    f"{EXPECTED_VICTORY_EXIT_CODE} -- worth investigating, not silently accepting",
                    file=sys.stderr,
                )
        else:
            print(f"# NOT a clean EXIT -- halted on {h['reason']}, this is a fault, not Victory", file=sys.stderr)

        # The .tsv itself is NOT committed (team lead's call on #129): ~700K-
        # 766K lines / 20-30 MB of derived data that changes with every ROM
        # doesn't belong in a source repo, and it's cheaply regenerable (the
        # whole point of the ~50 minute estimate). What's durable is the
        # *answer*: this manifest -- final icount, final fb_hash, exit_code,
        # the PINNED_HASH it was generated against, and the trace file's own
        # sha256, so regeneration is verifiable (rebuild in ~50 minutes,
        # compare the manifest) without needing the bytes themselves in git.
        # Contrast with #114's boot-to-first-frame trace, which IS committed
        # -- that one is 3,328 lines / 112 KB, small enough that the
        # convenience wins. Different sizes, same discipline applied
        # consistently, not an inconsistency between the two scripts.
        trace_bytes = tsv_path.read_bytes()
        trace_sha256 = hashlib.sha256(trace_bytes).hexdigest()
        stripped = trace_bytes.rstrip(b"\n")
        final_checkpoint = stripped.rsplit(b"\n", 1)[-1].decode() if stripped else None

        # The halt icount does NOT generally land on a RAM_HASH_INTERVAL
        # boundary (EXIT can fire anywhere), so the .tsv's own last
        # hash-bearing line can be stale by up to one interval -- exactly
        # what happened on the first real run this was used for (#129):
        # the .tsv's final line had no ramhash/fbhash at all, and manually
        # recomputing from the saved state was the only way to get README's
        # actual "final frame hash". Computed here directly from `cpu`
        # (still holds the exact halt-time state -- `run()` mutates it in
        # place, it isn't reloaded from disk) so every future run gets this
        # for free instead of requiring the same manual recovery.
        final_state = {
            "icount": cpu.icount,
            "pc": cpu.pc,
            "reghash": f"{reg_hash(cpu.pc, cpu.regs):016x}",
            "ramhash": f"{ram_hash(cpu.memory.ram):016x}",
            "fbhash": f"{fb_hash(cpu.memory.framebuffer, cpu.memory.palette):016x}",
        }
        frame_commits = cpu.memory.mmio.frame_commits
        last_frame_commit = (
            {"frame_no": frame_commits[-1][0], "committed_icount": frame_commits[-1][1]}
            if frame_commits
            else None
        )

        full_meta = {
            "spec_version": manifest.get("spec_version"),
            "rom_sha256": rom_sha256,
            "rom_manifest": manifest,
            "generated_by": "refemu/scripts/gen_demo3_trace.py " + " ".join(sys.argv[1:]),
            "trace_file": tsv_path.name,
            "trace_file_sha256": trace_sha256,
            "trace_file_bytes": tsv_path.stat().st_size,
            "final_checkpoint_line": final_checkpoint,
            "final_icount": summary["final_icount"],
            "final_state_at_halt": final_state,
            "frame_commit_count": len(frame_commits),
            "last_frame_commit": last_frame_commit,
            "halt": summary["halt"],
            "elapsed_seconds": summary["elapsed_seconds"],
            "checkpoint_interval": CHECKPOINT_INTERVAL,
            "ram_hash_interval": RAM_HASH_INTERVAL,
        }
        meta_path.write_text(json.dumps(full_meta, indent=2) + "\n")
        print(f"# wrote {meta_path} (manifest only -- .tsv is not committed, see script docstring)", file=sys.stderr)
        print(f"# final_state_at_halt: {final_state}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
