"""Tests for `scripts/gen_demo3_trace.py` (issue #129: the resumable
`demo3` reference harness, prepared but not run against the real ROM).

The one property that matters most: **resuming must be invisible in the
output.** A trace produced by run-to-completion-in-one-go and a trace
produced by run/interrupt/resume/continue (possibly several times) over
the exact same program must be byte-for-byte identical --
`test_interrupted_and_resumed_run_matches_single_run` is that proof, not
an inspection of the resume logic in isolation.
"""

from __future__ import annotations

import importlib.util
import os
import signal
import threading
import time
from pathlib import Path

from refemu.cpu import new_cpu
from refemu.memory import RAM_BASE

from .asm import addi, ecall

SCRIPT_PATH = Path(__file__).resolve().parent.parent / "scripts" / "gen_demo3_trace.py"


def _load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


demo3 = _load_module(SCRIPT_PATH, "gen_demo3_trace")


def _image_from_words(words: list[int]) -> bytes:
    return b"".join(w.to_bytes(4, "little") for w in words)


def _make_image(loop_iterations: int) -> bytes:
    # A long-enough addi loop, then ecall (fatal halt, but a real stop
    # condition -- Halted is what this harness actually watches for, not
    # EXIT specifically). Long enough that a background thread has time
    # to deliver a signal mid-run in the SIGINT test.
    words = [addi(1, 1, 1)] * loop_iterations + [ecall()]
    return _image_from_words(words)


def test_estimate_wall_clock_reports_a_range_not_one_number():
    est = demo3.estimate_wall_clock(1_000_000)
    assert est["low_estimate_seconds"] < est["high_estimate_seconds"]
    assert est["low_estimate_instructions"] == demo3.LOW_ESTIMATE_INSTRUCTIONS
    assert est["high_estimate_instructions"] == demo3.HIGH_ESTIMATE_INSTRUCTIONS
    # Sanity: at 1M instr/sec, the low estimate should be on the order of
    # 2,900 seconds (2.90e9 / 1e6), not minutes or years -- catches a
    # units bug (ms vs s, or a stray *1000) rather than trusting the
    # arithmetic by inspection alone.
    assert 2800 < est["low_estimate_seconds"] < 3000


def test_single_run_to_completion(tmp_path):
    image = _make_image(50_000)
    manifest = {"text_start": None, "text_end": None}
    cpu = new_cpu(text_start=manifest["text_start"], text_end=manifest["text_end"])
    cpu.memory.load_image(image, base=RAM_BASE)

    summary = demo3.run(
        cpu,
        tsv_path=tmp_path / "run.tsv",
        state_path=tmp_path / "run.state.pkl",
        progress_path=tmp_path / "run.progress.json",
        max_instructions=2**63,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=0.0,
        resumed_tsv_offset=0,
    )

    assert summary["halt"]["reason"] == "ECALL"
    assert not summary["stopped_early"]
    assert (tmp_path / "run.tsv").exists()
    assert (tmp_path / "run.state.pkl").exists()


def test_interrupted_and_resumed_run_matches_single_run(tmp_path):
    # Long enough to cross several CHECKPOINT_INTERVAL boundaries so the
    # comparison is meaningful (not just "both produced zero lines").
    from refemu.trace import CHECKPOINT_INTERVAL

    iterations = CHECKPOINT_INTERVAL * 5
    image = _make_image(iterations)
    manifest = {"text_start": None, "text_end": None}

    # Reference: one uninterrupted run.
    ref_dir = tmp_path / "ref"
    ref_dir.mkdir()
    cpu_ref = new_cpu(text_start=manifest["text_start"], text_end=manifest["text_end"])
    cpu_ref.memory.load_image(image, base=RAM_BASE)
    demo3.run(
        cpu_ref,
        tsv_path=ref_dir / "t.tsv",
        state_path=ref_dir / "t.state.pkl",
        progress_path=ref_dir / "t.progress.json",
        max_instructions=2**63,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=0.0,
        resumed_tsv_offset=0,
    )
    reference_trace = (ref_dir / "t.tsv").read_bytes()
    assert len(reference_trace) > 0

    # Interrupted: stop partway (max_instructions cap simulates a kill),
    # forcing a final state save via the "save on any stopping condition"
    # path (not the periodic wall-clock-gated one, which never fires
    # since checkpoint_every_seconds=1e18 here) -- then resume and finish.
    split_dir = tmp_path / "split"
    split_dir.mkdir()
    cpu_a = new_cpu(text_start=manifest["text_start"], text_end=manifest["text_end"])
    cpu_a.memory.load_image(image, base=RAM_BASE)
    first = demo3.run(
        cpu_a,
        tsv_path=split_dir / "t.tsv",
        state_path=split_dir / "t.state.pkl",
        progress_path=split_dir / "t.progress.json",
        max_instructions=CHECKPOINT_INTERVAL * 2,  # stop partway
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=0.0,
        resumed_tsv_offset=0,
    )
    assert first["halt"] is None  # stopped on the budget, not a real halt
    assert first["final_icount"] == CHECKPOINT_INTERVAL * 2

    state = demo3.load_state(split_dir / "t.state.pkl")
    assert state["icount"] == CHECKPOINT_INTERVAL * 2
    cpu_b = demo3.cpu_from_state(state, manifest["text_start"], manifest["text_end"])
    second = demo3.run(
        cpu_b,
        tsv_path=split_dir / "t.tsv",
        state_path=split_dir / "t.state.pkl",
        progress_path=split_dir / "t.progress.json",
        max_instructions=2**63,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=state["elapsed_seconds"],
        resumed_tsv_offset=state["tsv_byte_offset"],
    )
    assert second["halt"]["reason"] == "ECALL"

    split_trace = (split_dir / "t.tsv").read_bytes()
    assert split_trace == reference_trace


def test_resume_truncates_tsv_past_saved_offset(tmp_path):
    # Simulates a crash that left extra bytes in the .tsv beyond what the
    # last saved state accounts for (e.g. a checkpoint line written, then
    # the process died before the next state save) -- resuming must
    # discard that dangling tail, not double-count or corrupt it.
    from refemu.trace import CHECKPOINT_INTERVAL

    image = _make_image(CHECKPOINT_INTERVAL * 3)
    manifest = {"text_start": None, "text_end": None}
    cpu = new_cpu(text_start=manifest["text_start"], text_end=manifest["text_end"])
    cpu.memory.load_image(image, base=RAM_BASE)

    tsv_path = tmp_path / "t.tsv"
    state_path = tmp_path / "t.state.pkl"
    demo3.run(
        cpu,
        tsv_path=tsv_path,
        state_path=state_path,
        progress_path=tmp_path / "t.progress.json",
        max_instructions=CHECKPOINT_INTERVAL,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=0.0,
        resumed_tsv_offset=0,
    )
    state = demo3.load_state(state_path)
    good_offset = state["tsv_byte_offset"]

    # Simulate the dangling-tail crash: append garbage past the saved offset.
    with open(tsv_path, "ab") as f:
        f.write(b"99999999\tdeadbeef\tdeadbeefdeadbeef\n")
    assert tsv_path.stat().st_size > good_offset

    cpu_b = demo3.cpu_from_state(state, manifest["text_start"], manifest["text_end"])
    demo3.run(
        cpu_b,
        tsv_path=tsv_path,
        state_path=state_path,
        progress_path=tmp_path / "t.progress.json",
        max_instructions=CHECKPOINT_INTERVAL + 10,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=state["elapsed_seconds"],
        resumed_tsv_offset=good_offset,
    )
    content = tsv_path.read_text()
    assert "deadbeef" not in content


def test_save_state_is_atomic_no_tmp_left_behind(tmp_path):
    from refemu.trace import CHECKPOINT_INTERVAL

    image = _make_image(CHECKPOINT_INTERVAL)
    cpu = new_cpu()
    cpu.memory.load_image(image, base=RAM_BASE)
    while cpu.icount < CHECKPOINT_INTERVAL:
        cpu.step()

    state_path = tmp_path / "s.pkl"
    demo3.save_state(state_path, cpu, tsv_byte_offset=0, elapsed_seconds=1.0)
    assert state_path.exists()
    assert not state_path.with_suffix(".pkl.tmp").exists()

    loaded = demo3.load_state(state_path)
    assert loaded["icount"] == cpu.icount
    assert loaded["pc"] == cpu.pc
    assert loaded["ram"] == bytes(cpu.memory.ram)


def test_sigint_stops_cleanly_and_saves_state(tmp_path):
    from refemu.trace import CHECKPOINT_INTERVAL

    # Enough iterations to keep the run loop alive long enough for a
    # concurrently-delivered SIGINT to land before it would finish on
    # its own.
    iterations = CHECKPOINT_INTERVAL * 200
    image = _make_image(iterations)
    cpu = new_cpu()
    cpu.memory.load_image(image, base=RAM_BASE)

    def send_sigint_soon():
        time.sleep(0.05)
        os.kill(os.getpid(), signal.SIGINT)

    t = threading.Thread(target=send_sigint_soon)
    t.start()
    summary = demo3.run(
        cpu,
        tsv_path=tmp_path / "t.tsv",
        state_path=tmp_path / "t.state.pkl",
        progress_path=tmp_path / "t.progress.json",
        max_instructions=2**63,
        checkpoint_every_seconds=1e18,
        progress_every_seconds=1e18,
        resumed_elapsed_seconds=0.0,
        resumed_tsv_offset=0,
    )
    t.join()

    assert summary["stopped_early"] is True
    assert summary["halt"] is None
    assert 0 < summary["final_icount"] < iterations
    # State must have been saved on the way out, at the icount SIGINT
    # actually interrupted at -- not zero, not the full run.
    state = demo3.load_state(tmp_path / "t.state.pkl")
    assert state["icount"] == summary["final_icount"]
