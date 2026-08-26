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


def test_main_writes_manifest_with_trace_sha256_and_final_checkpoint(tmp_path, monkeypatch):
    # End-to-end through main(): on a real halt, the manifest (not the
    # .tsv) is the intended committed artifact -- verify it actually
    # carries a sha256 that matches the real .tsv bytes on disk, and the
    # real final checkpoint line, not placeholder/stale values.
    import hashlib
    import json as json_module
    import sys as sys_module

    from .asm import lui, sw

    mmio_base = 0x1000_0000
    words = [
        lui(1, mmio_base >> 12),
        addi(2, 0, 0),
        sw(1, 2, 0x10),  # FRAME_COMMIT, frame_no=0
        addi(2, 0, 7),
        sw(1, 2, 0x08),  # EXIT, code 7
    ]
    image = _image_from_words(words)
    image_path = tmp_path / "rom.bin"
    image_path.write_bytes(image)
    manifest_path = tmp_path / "manifest.json"
    manifest_path.write_text(json_module.dumps({"text_start": None, "text_end": None}))
    pinned_hash = hashlib.sha256(image).hexdigest()
    pinned_hash_path = tmp_path / "PINNED_HASH"
    pinned_hash_path.write_text(pinned_hash)

    monkeypatch.setattr(demo3, "REPO_ROOT", tmp_path)
    monkeypatch.setattr(
        sys_module,
        "argv",
        [
            "gen_demo3_trace",
            "--image",
            str(image_path),
            "--manifest",
            str(manifest_path),
            "--pinned-hash",
            str(pinned_hash_path),
        ],
    )
    exit_status = demo3.main()
    assert exit_status == 0

    out_dir = tmp_path / "refemu" / "reference_traces" / "demo3"
    tsv_path = out_dir / f"demo3.{pinned_hash[:12]}.tsv"
    meta_path = out_dir / f"demo3.{pinned_hash[:12]}.json"
    assert tsv_path.exists()
    assert meta_path.exists()

    meta = json_module.loads(meta_path.read_text())
    assert meta["halt"]["reason"] == "EXIT"
    assert meta["halt"]["exit_code"] == 7
    assert meta["trace_file_sha256"] == hashlib.sha256(tsv_path.read_bytes()).hexdigest()
    assert meta["trace_file_bytes"] == tsv_path.stat().st_size

    # final_state_at_halt: the fix for #129's own finding -- the halt
    # icount rarely lands on a RAM_HASH_INTERVAL boundary, so the .tsv's
    # last hash-bearing line can be stale by up to one interval. This
    # program's own EXIT lands at icount 5, nowhere near a
    # RAM_HASH_INTERVAL multiple, so if final_state_at_halt were being
    # read off the .tsv instead of computed fresh from the halted cpu,
    # it would be missing (no ramhash/fbhash on that line) exactly like
    # the real run that motivated this fix.
    from refemu.trace import fb_hash, ram_hash

    fs = meta["final_state_at_halt"]
    # 4, not 5: SPEC §1's fatal-halt-doesn't-retire rule -- the 5th
    # (EXIT) instruction never increments icount.
    assert fs["icount"] == 4
    # RAM is the loaded image followed by zeros; framebuffer/palette are
    # untouched (empty) in this synthetic run -- confirms the manifest's
    # hashes are independently reproducible from the real expected
    # content, not just "some string", by recomputing with the real hash
    # functions and comparing.
    expected_ram = bytearray(24 * 1024 * 1024)
    expected_ram[: len(image)] = image
    assert fs["ramhash"] == f"{ram_hash(bytes(expected_ram)):016x}"
    assert fs["fbhash"] == f"{fb_hash(bytes(64_000), bytes(768)):016x}"
    assert meta["frame_commit_count"] == 1
    assert meta["last_frame_commit"] == {"frame_no": 0, "committed_icount": 2}
