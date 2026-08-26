"""Tests for `scripts/gen_reference_trace.py` (issue #96's ask: a stored
SPEC §7 reference trace for the real ROM out to the first `FRAME_COMMIT`).

The one property that actually matters here: this script's own
periodic-checkpoint loop (which exists because it needs per-step milestone
observation `refemu.trace.iter_trace()` can't give it) must produce
line-for-line identical output to `iter_trace()`/`run_trace()`, the real
SPEC §7 emitter -- otherwise "the reference trace" and "what the real
emitter would produce" are quietly two different things. `test_
periodic_checkpoints_match_run_trace` is the proof; everything else here
is the milestone-detection logic (I_InitGraphics, FRAME_COMMIT), tested
against synthetic MMIO activity so it doesn't need the real ROM built.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import refemu.trace as trace_module
from refemu.cpu import new_cpu
from refemu.memory import RAM_BASE
from refemu.trace import CHECKPOINT_INTERVAL, run_trace

from .asm import addi, ecall, sw

SCRIPT_PATH = Path(__file__).resolve().parent.parent / "scripts" / "gen_reference_trace.py"


def _load_gen_reference_trace():
    spec = importlib.util.spec_from_file_location("gen_reference_trace", SCRIPT_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


gen_reference_trace = _load_gen_reference_trace()


def _image_from_words(words: list[int]) -> bytes:
    return b"".join(w.to_bytes(4, "little") for w in words)


def test_periodic_checkpoints_match_run_trace(monkeypatch):
    # Same interval-shrinking trick test_trace.py uses, applied to BOTH
    # copies of the constant: the script imports RAM_HASH_INTERVAL into its
    # own module namespace (a plain `from ... import`, not a live
    # attribute lookup), so patching only refemu.trace's copy would leave
    # the script's `generate()` using the real 1,048,576 while
    # `run_trace()` used the patched value -- silently comparing two runs
    # with different ram/fb-hash cadences instead of proving equivalence.
    monkeypatch.setattr(gen_reference_trace, "RAM_HASH_INTERVAL", CHECKPOINT_INTERVAL)
    monkeypatch.setattr(trace_module, "RAM_HASH_INTERVAL", CHECKPOINT_INTERVAL)

    words = [addi(1, 1, 1)] * (3 * CHECKPOINT_INTERVAL) + [ecall()]
    image = _image_from_words(words)
    manifest = {"text_start": None, "text_end": None}

    result = gen_reference_trace.generate(image, manifest, max_instructions=3 * CHECKPOINT_INTERVAL + 10)

    cpu = new_cpu()
    cpu.memory.load_image(image, base=RAM_BASE)
    cpu.pc = RAM_BASE
    expected_lines, expected_halt = run_trace(cpu, max_instructions=3 * CHECKPOINT_INTERVAL + 10)

    assert result["lines"] == expected_lines
    assert result["meta"]["halt"]["reason"] == expected_halt.reason
    assert result["meta"]["final_icount"] == cpu.icount


def test_frame_commit_milestone_recorded_at_checkpoint_convention_icount():
    mmio_base = 0x1000_0000
    from .asm import lui

    words = [
        lui(1, mmio_base >> 12),
        addi(2, 0, 7),  # frame_no = 7
        sw(1, 2, 0x10),  # FRAME_COMMIT offset (mmio.FRAME_COMMIT)
        addi(3, 3, 1),
        addi(3, 3, 1),
        ecall(),
    ]
    image = _image_from_words(words)
    manifest = {"text_start": None, "text_end": None}

    result = gen_reference_trace.generate(image, manifest, max_instructions=100)

    fc = result["meta"]["frame_commit"]
    assert fc is not None
    assert fc["frame_no"] == 7
    # FRAME_COMMIT is the 3rd instruction (index 2); icount after it retires
    # is 3 -- the checkpoint-style "instructions retired" convention, not
    # Mmio.frame_commits' own "icount before the store" (which would be 2).
    assert fc["icount"] == 3
    assert fc["committed_icount"] == 2


def test_init_graphics_milestone_uses_last_console_change_before_frame_commit():
    # Mirrors the real ROM's shape (confirmed against the actual ROM while
    # writing this script): PUTCHAR activity that finishes well before
    # FRAME_COMMIT should be reported at the icount of its *last* byte, not
    # the icount where the needle text first becomes a substring match
    # (which fires mid-print, before the console has actually settled).
    from .asm import lui

    mmio_base = 0x1000_0000
    needle = b"I_InitGraphics: framebuffer"

    words = [lui(1, mmio_base >> 12)]
    for b in needle:
        words.append(addi(2, 0, b))
        words.append(sw(1, 2, 0x0C))  # PUTCHAR offset
    words.append(addi(2, 0, 9))  # frame_no
    words.append(sw(1, 2, 0x10))  # FRAME_COMMIT
    words.append(ecall())

    image = _image_from_words(words)
    manifest = {"text_start": None, "text_end": None}
    result = gen_reference_trace.generate(image, manifest, max_instructions=1000)

    # Two instructions per byte (load + store); FRAME_COMMIT fires right
    # after the last byte's store instruction retires. init_graphics_icount
    # should land on that same instruction (the last console change), not
    # on the earlier instruction where the needle substring first completed
    # inside console_out (which is also the same instant here, since the
    # needle is exactly the whole message -- see the "distinct instants"
    # test below for where that distinction actually bites).
    assert result["meta"]["init_graphics_icount"] == result["meta"]["frame_commit"]["icount"] - 2


def test_init_graphics_milestone_distinguishes_needle_completion_from_last_change():
    # The real bug this logic was written to avoid (found generating the
    # actual reference trace against the real ROM, see the script's
    # docstring): the needle can complete mid-print, with more bytes still
    # to come in the same console block. init_graphics_icount must track
    # the *last* pre-FRAME_COMMIT console change, not the earlier instant
    # the needle substring first matched.
    from .asm import lui

    mmio_base = 0x1000_0000
    needle = b"I_InitGraphics: framebuffer"
    trailer = b": more text after the needle completes\n"

    words = [lui(1, mmio_base >> 12)]
    for b in needle + trailer:
        words += [addi(2, 0, b), sw(1, 2, 0x0C)]
    words.append(addi(2, 0, 3))
    words.append(sw(1, 2, 0x10))  # FRAME_COMMIT
    words.append(ecall())

    image = _image_from_words(words)
    manifest = {"text_start": None, "text_end": None}
    result = gen_reference_trace.generate(image, manifest, max_instructions=2000)

    fc_icount = result["meta"]["frame_commit"]["icount"]
    ig_icount = result["meta"]["init_graphics_icount"]
    # The last byte written is 2 instructions before FRAME_COMMIT (load +
    # store), same as the single-block test above -- confirming the
    # trailer's bytes (written *after* the needle substring completed) are
    # what set init_graphics_icount, not the earlier needle-completion
    # instant.
    assert ig_icount == fc_icount - 2
    # And that's strictly later than where the needle itself completed --
    # otherwise this test isn't actually exercising the distinction.
    needle_complete_instructions = 1 + 2 * len(needle)  # lui + (load+store) per byte
    needle_complete_icount = needle_complete_instructions
    assert ig_icount > needle_complete_icount
