#!/usr/bin/env python3
"""Synthetic sparse-framebuffer/palette fixture generator -- issue #220's
negative-test data.

`frame_readout_sql()` originally read `framebuffer`/`palette` with a bare
`groupArray(value) FROM (... FINAL ORDER BY word_addr)`, correct only
because `DG_DrawFrame` happens to write every word in both regions before
signalling `FRAME_COMMIT` (rom/src/dg_hooks.c) -- a property of the ROM's
behaviour at the one real call site, not something the query itself
enforces. Any unwritten word shortens the `groupArray` result instead of
reading as 0, shifting every later byte and producing a wrong `fb_hash`
with no error (#220's own repro section). This never shows up against
real DOOM output because DOOM never leaves either region genuinely sparse
at commit time -- so this script constructs that condition deliberately,
synthetically, rather than waiting for a ROM change that might trigger it
by accident.

Two independent cases (`--which fb|pal`), matching #220's own evidence
requirement: "A framebuffer-only test passes even if the palette operand
were dropped entirely." One case injects sparseness into FRAMEBUFFER only
(PALETTE left fully dense); the other injects it into PALETTE only
(FRAMEBUFFER left fully dense) -- so a fix that only covers one side
(e.g. `fb_words` switched to `dense_words_sql()` but `pal_words` left
bare) is still caught by the case it didn't fix.

No ClickHouse here, and no CPU stepping. This is synthetic deterministic
test data, not a real DOOM frame. `word_value()` below is a fixed
multiplicative hash rather than `random` or `hash()`, because CPython salts
`hash()` per process for several builtin types, which would make this
fixture different on every run.

The framebuffer hash comes from the emulator. There is one definition of it
and this file is not a second one.

Usage:
    python3 gen_sparse_frame_fixture.py --which fb --written-words 100 --out /tmp/sparse_fb.json
    python3 gen_sparse_frame_fixture.py --which pal --written-words 50  --out /tmp/sparse_pal.pkl
"""

from __future__ import annotations

import argparse
import os
import json
import subprocess  # purity-ok: fixture tooling, not the runtime driver loop; it asks refemu for the framebuffer hash rather than computing one
import tempfile
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Same word counts as driver/render.py's FRAMEBUFFER_WORDS/PALETTE_WORDS
# (SPEC §2) -- duplicated as plain literals, not imported, so this
# fixture generator has no dependency on the module it's used to test.
FRAMEBUFFER_WORDS = 16_000
PALETTE_WORDS = 192


def frame_hash(framebuffer: bytes, palette: bytes) -> str:
    """The pinned framebuffer hash, from the emulator that defines it.

    Shelling out rather than reimplementing: there is one definition of this
    hash and this file is not it.
    """
    with tempfile.TemporaryDirectory() as tmp:
        fb_path = Path(tmp) / "fb.bin"
        pal_path = Path(tmp) / "pal.bin"
        fb_path.write_bytes(framebuffer)
        pal_path.write_bytes(palette)
        result = subprocess.run(  # purity-ok: fixture tooling, not the runtime driver loop; delegating the hash to refemu is what keeps a second implementation of it out of driver/
            [
                os.environ.get("REFEMU", "./target/release/refemu"),
                "hash",
                "fb",
                "--framebuffer",
                str(fb_path),
                "--palette",
                str(pal_path),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
    return result.stdout.strip()


def word_value(addr: int) -> int:
    """Deterministic, non-zero pseudo-value per address -- a fixed
    multiplicative hash (Knuth's constant), not `random`/`hash()`."""
    return ((addr + 1) * 2654435761) & 0xFFFFFFFF


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--which", choices=["fb", "pal"], required=True)
    ap.add_argument("--written-words", type=int, required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    dense_fb = [word_value(a) for a in range(FRAMEBUFFER_WORDS)]
    dense_pal = [word_value(a) for a in range(PALETTE_WORDS)]

    if args.which == "fb":
        total = FRAMEBUFFER_WORDS
        if not (0 < args.written_words < total):
            print(f"::error::--written-words must leave a genuinely unwritten tail (0 < n < {total})", file=sys.stderr)
            return 1
        fb_rows = [(a, dense_fb[a]) for a in range(args.written_words)]
        pal_rows = [(a, dense_pal[a]) for a in range(PALETTE_WORDS)]  # fully dense -- isolates the fb half
        expected_fb = b"".join(
            (dense_fb[a] if a < args.written_words else 0).to_bytes(4, "little") for a in range(FRAMEBUFFER_WORDS)
        )
        expected_pal = b"".join(v.to_bytes(4, "little") for v in dense_pal)
    else:
        total = PALETTE_WORDS
        if not (0 < args.written_words < total):
            print(f"::error::--written-words must leave a genuinely unwritten tail (0 < n < {total})", file=sys.stderr)
            return 1
        fb_rows = [(a, dense_fb[a]) for a in range(FRAMEBUFFER_WORDS)]  # fully dense -- isolates the palette half
        pal_rows = [(a, dense_pal[a]) for a in range(args.written_words)]
        expected_fb = b"".join(v.to_bytes(4, "little") for v in dense_fb)
        expected_pal = b"".join(
            (dense_pal[a] if a < args.written_words else 0).to_bytes(4, "little") for a in range(PALETTE_WORDS)
        )

    expected_fbhash = frame_hash(expected_fb, expected_pal)
    state = {
        "which": args.which,
        "written_words": args.written_words,
        "fb_rows": fb_rows,   # only the rows to actually INSERT -- the rest is the deliberate gap
        "pal_rows": pal_rows,
        "expected_fbhash": expected_fbhash,
    }
    state["schema"] = "clickdoom.sparse-frame-fixture/1"
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(state, indent=2) + "\n")
    print(
        f"# which={args.which} written_words={args.written_words}/{total} expected_fbhash={expected_fbhash}",
        file=sys.stderr,
    )
    print(f"# wrote {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
