"""SPEC §7 trace emitter tests (issue #15).

The `test_*_matches_clickhouse` cases hardcode hash values that were cross
-checked against real ClickHouse 26.3 (the repo pin), not just against
Python's own xxhash library agreeing with itself. Reproduce against a
running `make up` instance:

    docker exec clickdoom-ch clickhouse-client -q \\
      "SELECT xxHash64(unhex('<hex from the test>'))"

This is the actual evidence for issue #15's "coordinate the trace format
with sqlcpu" requirement -- these numbers are what sqlcpu's own xxHash64
usage has to reproduce, not just this file's assumptions about it.
"""

import json
import sys

from refemu.cpu import CPU, HaltReason
from refemu.memory import RAM_BASE, Memory
from refemu.mmio import EXIT
from refemu.trace import (
    CHECKPOINT_INTERVAL,
    RAM_HASH_INTERVAL,
    _main,
    fb_hash,
    format_checkpoint,
    iter_trace,
    ram_hash,
    reg_hash,
    run_trace,
)

from .asm import addi, ecall, lui, sw
from .conftest import load


def test_reg_hash_matches_clickhouse_pc_only():
    # pc = 0x80000004, all 31 hashed registers zero. Buffer:
    # 04 00 00 80 followed by 124 zero bytes.
    # ClickHouse: SELECT xxHash64(unhex('0400008000000...(124 zero bytes)'))
    assert reg_hash(0x8000_0004, [0] * 32) == 4903144380889844081


def test_reg_hash_matches_clickhouse_nonzero_regs():
    regs = [0] * 32
    regs[1] = 0xDEADBEEF
    regs[10] = 42
    regs[31] = 0xFFFFFFFF
    # ClickHouse: SELECT xxHash64(unhex(
    #   '00010080efbeadde0000...(zeros)...2a0000...(zeros)...ffffffff'))
    assert reg_hash(0x8000_0100, regs) == 11036197505622382625


def test_reg_hash_ignores_x0():
    # x0 is never hashed (SPEC §1: always 0 by construction) -- poking
    # index 0 must not change the digest.
    regs = [0] * 32
    regs[0] = 0xFFFFFFFF  # would corrupt the hash if accidentally included
    assert reg_hash(0x8000_0004, regs) == 4903144380889844081


def test_ram_hash_matches_clickhouse():
    ram = bytes(i % 256 for i in range(64))
    # ClickHouse: SELECT xxHash64(unhex('000102...3e3f'))
    assert ram_hash(ram) == 17854084224570037232


def test_fb_hash_matches_clickhouse():
    fb = bytes(range(16))
    palette = bytes(range(200, 208))
    # ClickHouse: SELECT xxHash64(unhex('000102030405060708090a0b0c0d0e0fc8c9cacbcccdcecf'))
    assert fb_hash(fb, palette) == 10814741248291066246


def test_fb_hash_order_matters():
    # framebuffer || palette, not the other way around -- swapping them
    # must not produce the same digest (guards against an accidental
    # concat-order regression, which xxh64 gives no other signal about).
    fb = bytes(range(16))
    palette = bytes(range(200, 208))
    assert fb_hash(fb, palette) != fb_hash(palette, fb)


def test_format_checkpoint_no_ram_hash():
    line = format_checkpoint(4096, 0x8000_1000, 0x1234_5678_9ABC_DEF0)
    assert line == "4096\t80001000\t123456789abcdef0"


def test_format_checkpoint_with_ram_hash():
    line = format_checkpoint(1_048_576, 0x8000_2000, 0xFF, 0xAB)
    assert line == "1048576\t80002000\t00000000000000ff\t00000000000000ab"


def test_format_checkpoint_with_fb_hash():
    line = format_checkpoint(1_048_576, 0x8000_2000, 0xFF, 0xAB, 0xCD)
    assert line == "1048576\t80002000\t00000000000000ff\t00000000000000ab\t00000000000000cd"


def test_format_checkpoint_fb_hash_requires_ram_hash_column_present():
    # fbhash is only ever meaningful alongside ramhash (same cadence,
    # issue #55/#56) -- passing fbhash without ramhash would produce a
    # malformed line (fbhash landing in the ramhash column position).
    # format_checkpoint doesn't forbid it, but callers (run_trace/
    # iter_trace) never do this; document the real shape instead.
    line = format_checkpoint(1, 0, 0, ramhash=0, fbhash=0)
    assert line.count("\t") == 4  # icount, pc, reghash, ramhash, fbhash


def test_format_checkpoint_lowercase_zero_padded():
    # Regression guard for the exact detail issue #15 calls out: this must
    # never silently become uppercase or unpadded.
    line = format_checkpoint(1, 0, 0)
    assert line == "1\t00000000\t0000000000000000"


def test_run_trace_checkpoint_boundaries(cpu):
    # A tight loop that retires exactly CHECKPOINT_INTERVAL instructions,
    # then halts. Expect exactly one checkpoint line, at that boundary.
    words = [addi(1, 1, 1)] * CHECKPOINT_INTERVAL + [ecall()]
    load(cpu, words)
    lines, halt = run_trace(cpu, max_instructions=CHECKPOINT_INTERVAL + 10)
    assert len(lines) == 1
    icount_str, _pc_hex, _reghash_hex = lines[0].split("\t")
    assert icount_str == str(CHECKPOINT_INTERVAL)
    assert halt is not None
    assert halt.reason == HaltReason.ECALL


def test_run_trace_ram_hash_only_at_ram_hash_interval(cpu):
    # Two checkpoints' worth of instructions, no ram hash expected on the
    # first (not a RAM_HASH_INTERVAL boundary), only relevant when icount
    # actually reaches RAM_HASH_INTERVAL -- verified structurally here
    # since actually retiring 1,048,576 instructions in a unit test would
    # be wasteful; the boundary arithmetic is exercised directly instead.
    words = [addi(1, 1, 1)] * (2 * CHECKPOINT_INTERVAL) + [ecall()]
    load(cpu, words)
    lines, halt = run_trace(cpu, max_instructions=2 * CHECKPOINT_INTERVAL + 10)
    assert len(lines) == 2
    for line in lines:
        assert len(line.split("\t")) == 3  # no ram hash column
    assert halt is not None


def test_run_trace_stops_without_halt_at_max_instructions(cpu):
    words = [addi(1, 1, 1)] * (CHECKPOINT_INTERVAL * 3)
    load(cpu, words)
    lines, halt = run_trace(cpu, max_instructions=CHECKPOINT_INTERVAL)
    assert len(lines) == 1
    assert halt is None  # ran out of budget, did not fault or exit


def test_iter_trace_matches_run_trace(cpu):
    words = [addi(1, 1, 1)] * CHECKPOINT_INTERVAL
    load(cpu, words)
    streamed = list(iter_trace(cpu, CHECKPOINT_INTERVAL))

    cpu2 = CPU(memory=Memory())
    load(cpu2, words)
    batched, _ = run_trace(cpu2, CHECKPOINT_INTERVAL)

    assert streamed == batched


def test_ram_hash_interval_is_multiple_of_checkpoint_interval():
    # Load-bearing assumption in run_trace/iter_trace's boundary check.
    assert RAM_HASH_INTERVAL % CHECKPOINT_INTERVAL == 0


def test_run_trace_threads_fb_hash_from_real_cpu_memory(cpu, monkeypatch):
    # Retiring a real RAM_HASH_INTERVAL's worth of instructions (1,048,576)
    # just to see a ramhash/fbhash column would be wasteful (same reasoning
    # as test_run_trace_ram_hash_only_at_ram_hash_interval above), so lower
    # the interval to CHECKPOINT_INTERVAL for this test only -- this still
    # exercises the real code path (run_trace reading cpu.memory.framebuffer/
    # palette), not just fb_hash() in isolation.
    import refemu.trace as trace_module

    monkeypatch.setattr(trace_module, "RAM_HASH_INTERVAL", CHECKPOINT_INTERVAL)

    cpu.memory.framebuffer[0] = 0xAB
    cpu.memory.palette[0] = 0xCD
    words = [addi(1, 1, 1)] * CHECKPOINT_INTERVAL + [ecall()]
    load(cpu, words)
    lines, halt = trace_module.run_trace(cpu, max_instructions=CHECKPOINT_INTERVAL + 10)

    assert len(lines) == 1
    _icount_str, _pc_hex, _reghash_hex, _ramhash_hex, fbhash_hex = lines[0].split("\t")
    expected = fb_hash(cpu.memory.framebuffer, cpu.memory.palette)
    assert fbhash_hex == f"{expected:016x}"
    assert halt is not None


def test_main_cli_wires_real_mmio_so_exit_halts(tmp_path, monkeypatch, capsys):
    # Issue #94: an earlier version of _main() built its CPU with a bare
    # Memory() (NullMmio), so a real image's EXIT write landed as an inert
    # byte store instead of halting -- exactly wrong for the CLI whose own
    # docstring says it's the interface a real differential run drives.
    # This image sets MMIO base in x1 and writes EXIT with code 7: if MMIO
    # is real, it halts on the third instruction; if it silently fell back
    # to NullMmio storage, it would run to --max-instructions instead.
    mmio_base = 0x1000_0000
    words = [
        lui(1, mmio_base >> 12),
        addi(2, 0, 7),
        sw(1, 2, EXIT),
    ]
    image = b"".join(w.to_bytes(4, "little") for w in words)
    image_path = tmp_path / "exit.bin"
    image_path.write_bytes(image)

    monkeypatch.setattr(sys, "argv", ["refemu", str(image_path), "--max-instructions", "100"])
    exit_status = _main()

    captured = capsys.readouterr()
    assert exit_status == 1  # halted (mirrors boot.py's bucketing -- see _main's docstring)
    assert captured.out == ""  # halted before any checkpoint -- 3 instructions, CHECKPOINT_INTERVAL=4096
    assert "halted: EXIT" in captured.err
    assert "exit_code=7" in captured.err


def test_main_cli_reads_text_bounds_from_manifest(tmp_path, monkeypatch, capsys):
    # Mirrors boot.py's --manifest convention exactly (same flag, same
    # auto-discovery next to the image, same text_start/text_end fields)
    # so issue #27's differential harness can point both CLIs at one
    # manifest.json without two conventions for finding it. A store into
    # the declared text region should raise SelfModify -> SELF_MODIFY,
    # which only happens if the manifest's bounds actually reached the CPU.
    words = [
        lui(1, RAM_BASE >> 12),  # x1 = RAM_BASE
        addi(2, 0, 0),  # x2 = 0
        sw(1, 2, 0),  # store x2 -> [x1 + 0] == RAM_BASE -- inside the manifest's text region below
    ]
    image = b"".join(w.to_bytes(4, "little") for w in words)
    image_dir = tmp_path
    image_path = image_dir / "self_modify.bin"
    image_path.write_bytes(image)
    (image_dir / "manifest.json").write_text(
        json.dumps({"text_start": RAM_BASE, "text_end": RAM_BASE + 0x1000})
    )

    monkeypatch.setattr(sys, "argv", ["refemu", str(image_path), "--max-instructions", "100"])
    exit_status = _main()

    captured = capsys.readouterr()
    assert exit_status == 1
    assert f"# manifest: {image_dir / 'manifest.json'}" in captured.err
    assert "halted: SELF_MODIFY" in captured.err


def test_main_cli_explicit_text_bounds_override_manifest(tmp_path, monkeypatch, capsys):
    # --text-start/--text-end are a deliberate override, not shadowed by a
    # manifest.json that happens to sit next to the image.
    words = [
        lui(1, RAM_BASE >> 12),  # x1 = RAM_BASE
        addi(2, 0, 0),  # x2 = 0
        sw(1, 2, 0),  # store x2 -> [x1 + 0] == RAM_BASE
    ]
    image = b"".join(w.to_bytes(4, "little") for w in words)
    image_dir = tmp_path
    image_path = image_dir / "self_modify.bin"
    image_path.write_bytes(image)
    # Manifest declares a text region that does NOT cover RAM_BASE.
    (image_dir / "manifest.json").write_text(
        json.dumps({"text_start": RAM_BASE + 0x2000, "text_end": RAM_BASE + 0x3000})
    )

    monkeypatch.setattr(
        sys,
        "argv",
        [
            "refemu",
            str(image_path),
            "--max-instructions",
            "100",
            "--text-start",
            hex(RAM_BASE),
            "--text-end",
            hex(RAM_BASE + 0x1000),
        ],
    )
    exit_status = _main()

    captured = capsys.readouterr()
    assert exit_status == 1
    assert "halted: SELF_MODIFY" in captured.err
