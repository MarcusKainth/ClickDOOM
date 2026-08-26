#!/usr/bin/env python3
"""Load a `gen_frame_fixture.py` dump into an isolated database's
`framebuffer`/`palette`/`batch_commit` fixture tables (`fixture_schema.sql`
in this directory), so `render.py`'s frame_readout_sql() has real,
known-correct data to reconstruct.

Same reinterpret-bytes-as-words, insert-at-address technique as
`sqlcpu/load_rom.py` -- reinterpreting already-computed bytes, never
inspecting what a word *means*. Word-addressed relative to each region's
own base (0..15,999 for FRAMEBUFFER, 0..191 for PALETTE), per sqlcpu's
#130 design comment -- confirmed with sqlcpu-2 before use.

Usage:
    python3 seed_frame_fixture.py --fixture /tmp/frame_fixture.pkl \\
        --database driver_render_test --client "docker exec -i clickdoom-ch clickhouse-client"
"""

from __future__ import annotations

import argparse
import pickle
import struct
import subprocess  # purity-ok: shells out to clickhouse-client to load already-computed fixture bytes, same "housekeeping that computes nothing" class as sqlcpu/load_rom.py -- this script is test/fixture tooling, not the real runtime driver loop
import sys


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixture", required=True)
    ap.add_argument("--host", default="localhost")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--database", required=True)
    ap.add_argument("--client", default="clickhouse-client")
    args = ap.parse_args()

    with open(args.fixture, "rb") as f:
        state = pickle.load(f)

    fb, palette = state["framebuffer"], state["palette"]
    if len(fb) % 4 != 0 or len(palette) % 4 != 0:
        print(f"::error::framebuffer ({len(fb)} B) / palette ({len(palette)} B) not word-multiples", file=sys.stderr)
        return 1

    base_cmd = args.client.split() + [
        "--host", args.host, "--port", str(args.port),
        "--user", args.user, "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    def load_words(table: str, data: bytes, version: int) -> int | None:
        nwords = len(data) // 4
        words = struct.unpack(f"<{nwords}I", data)
        rows = "\n".join(f"{i}\t{w}\t{version}" for i, w in enumerate(words))
        insert = base_cmd + ["--query", f"INSERT INTO {table} (word_addr, value, version) FORMAT TSV"]
        result = subprocess.run(insert, input=rows, text=True, check=False)  # purity-ok: loads already-computed word bytes via clickhouse-client, computes nothing (fixture tooling, not the runtime driver)
        return result.returncode

    for table, data in (("framebuffer", fb), ("palette", palette)):
        rc = load_words(table, data, state["committed_icount"])
        if rc != 0:
            return rc
        print(f"seeded {table}: {len(data) // 4} words", file=sys.stderr)

    insert_batch_commit = base_cmd + [
        "--query",
        (
            "INSERT INTO batch_commit "
            "(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
            " keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
            f"VALUES (1, {state['committed_icount']}, 0, [], 0, '', 0, "
            f" 0, 1, {state['frame_no']}, [], [], [], [])"
        ),
    ]
    result = subprocess.run(insert_batch_commit, text=True, check=False)  # purity-ok: fixed-literal insert of already-known values, computes nothing (fixture tooling, not the runtime driver)
    if result.returncode != 0:
        return result.returncode
    print(f"seeded batch_commit: frame_no={state['frame_no']} icount={state['committed_icount']} has_frame=1",
          file=sys.stderr)
    print(f"expected fbhash (computed by gen_frame_fixture.py from refemu directly): {state['fbhash']}",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
