#!/usr/bin/env python3
"""Load a `gen_sparse_frame_fixture.py` dump into an isolated database's
`framebuffer`/`palette`/`batch_commit` fixture tables (issue #220's
negative test). Seeds ONLY the rows the fixture says were actually
written -- the whole point is to leave a genuine, deliberate gap in the
table so `frame_readout_sql()`'s density (or lack of it) can be observed.

Same TSV-insert technique as `seed_frame_fixture.py` in this directory
(word_addr/value/version columns, explicit-column INSERT so unlisted
Array columns on `batch_commit` default to `[]`) -- not reimplemented,
just applied to a sparse row list instead of a full contiguous blob.

Usage:
    python3 seed_sparse_fixture.py --fixture /tmp/sparse_fb.pkl \\
        --database driver_render_test --frame-no 1 --icount 1 \\
        --client "clickhouse-client"
"""

from __future__ import annotations

import argparse
import pickle
import subprocess  # purity-ok: shells out to clickhouse-client to load already-computed fixture rows, same "housekeeping that computes nothing" class as seed_frame_fixture.py -- test/fixture tooling, not the runtime driver
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
    ap.add_argument("--frame-no", type=int, required=True)
    ap.add_argument("--icount", type=int, required=True)
    ap.add_argument("--version", type=int, default=1)
    args = ap.parse_args()

    with open(args.fixture, "rb") as f:
        state = pickle.load(f)

    base_cmd = args.client.split() + [
        "--host", args.host, "--port", str(args.port),
        "--user", args.user, "--database", args.database,
    ]
    if args.password:
        base_cmd += ["--password", args.password]

    def load_words(table: str, rows: list[tuple[int, int]]) -> int:
        if not rows:
            print(f"seeded {table}: 0 words (deliberately empty)", file=sys.stderr)
            return 0
        tsv = "\n".join(f"{addr}\t{value}\t{args.version}" for addr, value in rows)
        insert = base_cmd + ["--query", f"INSERT INTO {table} (word_addr, value, version) FORMAT TSV"]
        result = subprocess.run(insert, input=tsv, text=True, check=False)  # purity-ok: loads already-computed word rows via clickhouse-client, computes nothing
        if result.returncode == 0:
            print(f"seeded {table}: {len(rows)} words (sparse by design)", file=sys.stderr)
        return result.returncode

    for table, rows in (("framebuffer", state["fb_rows"]), ("palette", state["pal_rows"])):
        rc = load_words(table, rows)
        if rc != 0:
            return rc

    insert_batch_commit = base_cmd + [
        "--query",
        (
            "INSERT INTO batch_commit "
            "(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
            " keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
            f"VALUES (1, {args.icount}, 0, [], 0, '', 0, "
            f" 0, 1, {args.frame_no}, [], [], [], [])"
        ),
    ]
    result = subprocess.run(insert_batch_commit, text=True, check=False)  # purity-ok: fixed-literal insert of already-known values
    if result.returncode != 0:
        return result.returncode
    print(f"seeded batch_commit: frame_no={args.frame_no} icount={args.icount} has_frame=1", file=sys.stderr)
    print(f"which={state['which']} written_words={state['written_words']} expected_fbhash={state['expected_fbhash']}",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
