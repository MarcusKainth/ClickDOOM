"""Seed an isolated ClickHouse database's `ram`/`framebuffer`/`palette`/
`batch_commit` from a `gen_snapshot.py` dump, so `run.sh` -- or a
frame-verification run (#251) -- can start from a real, representative
mid-run state without live-executing the tens of millions of instructions
it takes to reach it.

Four inserts, deliberately kept as dumb as `sqlcpu/load_rom.py`'s own ROM
load -- reinterpreting already-computed bytes and inserting them at their
address, never inspecting what a word *means*:

1. `ram`: the snapshot's full 24 MiB RAM dump, one row per word --
   `load_rom.py`'s exact TSV-over-stdin technique (the only way to move a
   few million rows in one client round trip), just from the snapshot's
   dense array instead of `image + zero-fill`. The snapshot IS already
   dense over the whole RAM region (refemu's `Memory.ram` is a flat
   `bytearray(RAM_SIZE)` from construction), so there is no separate
   zero-fill step and no density gap to hit #81's bug -- it is checked
   anyway, the same way `load_rom.py` checks its own load, because a
   single dropped byte-range here would silently feed the wrong RAM state
   into every subsequent measurement.
2. `framebuffer`/`palette` (#251): same technique and same density check
   as `ram` immediately above -- reused, not reinvented, because the
   underlying shape is identical (a dense byte blob -> one row per word,
   checked for gaps the same way). The one thing that must NOT be reused
   from `ram`'s handling is the RAM_BASE rebasing: `ram.word_addr` is
   absolute (byte address >> 2), but `framebuffer`/`palette`.word_addr are
   each region's own base-relative convention with NO rebasing step
   (`executor/commit.py:fbpal_flush_sql()`'s docstring spells this
   asymmetry out explicitly, and it exists precisely because an earlier
   draft of the analogous `ram` flush got the rebasing wrong -- #81).
   `gen_snapshot.py` already captures `cpu.memory.framebuffer`/`.palette`
   in that same region-relative form (byte 0 = the region's own base), so
   this function seeds them at `word_addr = i` directly, base_word=0 --
   getting this right by construction rather than by a rebasing formula
   that could be gotten backwards the way #81 was.
3. `batch_commit`'s batch_id=0 row: same shape as `executor/bootstrap.py`'s
   SPEC §1 reset-state insert, with the snapshot's icount/pc/regs instead
   of the reset values. NOT a change to `bootstrap.py` itself --
   `bootstrap.py`'s docstring is explicit that it seeds *reset* state and
   is meant to run "once, ever, per fresh run"; this is a different
   contract (an arbitrary mid-run snapshot, benchmark-only), so it gets
   its own insert rather than overloading `bootstrap.py`'s narrower one.
   `wl_addr`/`wl_val`/`wl_icount`/`console_bytes` are seeded empty and
   `keyq_pos`/`has_frame`/`frame_no` at 0 -- `console_out`/the key queue
   still have no SQL storage this benchmark needs (see `gen_snapshot.py`'s
   docstring), and this insert only needs pc/regs/ram/framebuffer/palette
   to measure throughput or verify a frame, not exact MMIO continuity.

## Format version (#251)

Refuses to run against a snapshot whose `state["format_version"]` is
missing or does not equal `snapshot_format.FORMAT_VERSION` -- a pre-#251
(format 1) snapshot has no `framebuffer`/`palette` keys at all, and
seeding one under this script's current assumptions would either KeyError
(if this script blindly indexed the missing keys) or, worse, silently skip
seeding those tables and leave them empty with no error -- exactly the
"clean, plausible, WRONG fb_hash" failure mode #251 exists to close off.
Checked before touching the database, not after: cheaper to fail loudly
before any INSERT than to leave a half-seeded database for the caller to
notice is wrong.

Usage:
    python3 seed_snapshot.py --snapshot /tmp/.../snapshot.9a6a47d01119.233932753.v2.pkl \\
        --database rom_bench_gameplay --client "docker exec -i clickdoom-ch clickhouse-client"
"""

from __future__ import annotations

import argparse
import pickle
import struct
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent  # rom/bench/canonical_throughput/ -> repo root
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "driver"))

from snapshot_format import FORMAT_VERSION  # noqa: E402

# FRAMEBUFFER_WORDS/PALETTE_WORDS from driver/render.py, not re-declared --
# that module is the SQL-side render query's own authority on these two
# constants (SPEC §2: 16,000/192), and `verify_snapshot_pixel_coverage.py`
# already established the pattern of reusing them from there rather than a
# second hardcoded copy.
import render  # noqa: E402


def seed_word_table(base_cmd: list[str], table: str, words: tuple[int, ...], base_word: int) -> int:
    """INSERT one row per word into `table` (`ram`/`framebuffer`/`palette`
    all share this exact shape -- word_addr/value/version, ReplacingMergeTree
    keyed on word_addr) and verify the result is dense over
    `[base_word, base_word + len(words))` with no gaps -- #81's bug is what a
    positionally-indexed reader does with a table that silently isn't dense,
    so this check runs for every word table seeded here, not only `ram`.
    Returns 0 on success, a nonzero returncode otherwise (caller propagates
    it as this script's exit code, same convention as every other
    `subprocess.run` call in this file)."""
    rows = "\n".join(f"{base_word + i}\t{w}\t0" for i, w in enumerate(words))
    insert = base_cmd + ["--query", f"INSERT INTO {table} (word_addr, value, version) FORMAT TSV"]
    result = subprocess.run(insert, input=rows, text=True)
    if result.returncode != 0:
        return result.returncode
    print(f"seeded {table}: {len(words)} words at word_addr {base_word}..{base_word + len(words) - 1}",
          file=sys.stderr)

    check = subprocess.run(
        base_cmd + [
            "--query", f"SELECT count(), max(word_addr) - min(word_addr) + 1, min(word_addr) FROM {table} FINAL",
        ],
        text=True, capture_output=True,
    )
    if check.returncode != 0:
        print(check.stderr, file=sys.stderr, end="")
        return check.returncode
    rows_n, span, lowest = (int(x) for x in check.stdout.split())
    if rows_n != span or rows_n != len(words) or lowest != base_word:
        print(f"::error::seeded {table} is not dense: {rows_n} rows spanning {span} words from {lowest}, "
              f"expected {len(words)} rows spanning {len(words)} from {base_word}. Readers of this table "
              f"index positionally -- see #81.", file=sys.stderr)
        return 1
    print(f"{table} dense: {rows_n} rows, word_addr {lowest}..{lowest + rows_n - 1} -- OK", file=sys.stderr)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--snapshot", required=True)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", required=True)
    ap.add_argument("--client", default="clickhouse-client")
    args = ap.parse_args()

    with open(args.snapshot, "rb") as f:
        state = pickle.load(f)

    # #251: refuse an old-format snapshot rather than silently seeding an
    # empty framebuffer/palette. Checked before any INSERT runs -- see this
    # module's docstring.
    snapshot_version = state.get("format_version")
    if snapshot_version != FORMAT_VERSION:
        print(f"::error::{args.snapshot}: format_version={snapshot_version!r}, expected {FORMAT_VERSION} -- "
              f"this snapshot predates #251 (or is otherwise the wrong shape) and has no "
              f"framebuffer/palette captured. Seeding it would leave those tables empty with no error, "
              f"producing a clean but WRONG fb_hash for any frame-verification run. Regenerate it with "
              f"the current gen_snapshot.py.", file=sys.stderr)
        return 1

    ram_bytes = state["ram"]
    if len(ram_bytes) % 4 != 0:
        print(f"::error::snapshot ram is {len(ram_bytes)} bytes, not a multiple of 4", file=sys.stderr)
        return 1
    ram_base = state["ram_base"]
    ram_base_word = ram_base >> 2
    ram_nwords = len(ram_bytes) // 4
    ram_words = struct.unpack(f"<{ram_nwords}I", ram_bytes)

    fb_bytes = state["framebuffer"]
    pal_bytes = state["palette"]
    if len(fb_bytes) != render.FRAMEBUFFER_WORDS * 4:
        print(f"::error::snapshot framebuffer is {len(fb_bytes)} bytes, expected "
              f"{render.FRAMEBUFFER_WORDS * 4} (SPEC §2)", file=sys.stderr)
        return 1
    if len(pal_bytes) != render.PALETTE_WORDS * 4:
        print(f"::error::snapshot palette is {len(pal_bytes)} bytes, expected "
              f"{render.PALETTE_WORDS * 4} (SPEC §2)", file=sys.stderr)
        return 1
    fb_words = struct.unpack(f"<{render.FRAMEBUFFER_WORDS}I", fb_bytes)
    pal_words = struct.unpack(f"<{render.PALETTE_WORDS}I", pal_bytes)

    base_cmd = args.client.split() + [
        "--host", args.host,
        "--port", str(args.port),
        "--user", args.user,
        "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    # Same TSV-over-stdin technique as sqlcpu/load_rom.py, same reason:
    # this is a few million rows, well past what `--query`'s ARG_MAX can
    # carry as literal SQL. `ram` is RAM_BASE-relative (base_word = ram_base
    # >> 2); `framebuffer`/`palette` are each region's own base-relative
    # convention with base_word=0 -- NOT ram_base, and NOT
    # FRAMEBUFFER_BASE/PALETTE_BASE >> 2 either (see this module's
    # docstring and `executor/commit.py:fbpal_flush_sql()`).
    rc = seed_word_table(base_cmd, "ram", ram_words, ram_base_word)
    if rc != 0:
        return rc
    print(f"  (icount={state['icount']:,})", file=sys.stderr)

    rc = seed_word_table(base_cmd, "framebuffer", fb_words, 0)
    if rc != 0:
        return rc
    rc = seed_word_table(base_cmd, "palette", pal_words, 0)
    if rc != 0:
        return rc

    regs31 = state["regs"][1:32]  # drop refemu's x0 (always 0, not stored -- schema.sql's convention)
    if len(regs31) != 31:
        print(f"::error::snapshot regs has {len(state['regs'])} elements, expected 32 (x0..x31)", file=sys.stderr)
        return 1
    regs_literal = "[" + ",".join(str(r) for r in regs31) + "]"
    insert_batch_commit = base_cmd + [
        "--query",
        "INSERT INTO batch_commit "
        "(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
        " keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
        f"VALUES (0, {state['icount']}, {state['pc']}, {regs_literal}, 0, '', 0, "
        f" 0, 0, 0, [], [], [], [])",
    ]
    result = subprocess.run(insert_batch_commit, text=True)
    if result.returncode != 0:
        return result.returncode
    print(f"seeded batch_commit batch_id=0: pc={state['pc']:#x}, icount={state['icount']:,}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
