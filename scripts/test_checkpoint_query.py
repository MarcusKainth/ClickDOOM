#!/usr/bin/env python3
"""Correctness test for scripts/checkpoint_query.py's two accessors --
issue #227/#230.

checkpoint_sql() (full, RAM_HASH_INTERVAL cadence) and reg_checkpoint_sql()
(cheap, every CHECKPOINT_INTERVAL) MUST compute reghash from the identical
expression. diff_run.sh compares the register-only cadence 256x more often
than the full one -- if the two accessors ever computed reghash slightly
differently, every one of those extra comparisons would read as a false
CPU divergence, the same misdiagnosis preflight_milestone.sh's ROM sha256
gate (gate 3) exists to head off on the ROM side (a run against the wrong
binary looking like a real divergence hours in).

Two levels of proof, run in this order:

1. STATIC (no ClickHouse needed, runs anywhere `python3` does): both
   accessors' generated SQL text must contain the identical substring
   produced by ONE call to sqlcpu/checkpoint.py's `reg_hash(pc="pc",
   regs="regs")` -- not "two expressions that happen to evaluate the same
   today", a textual identity check that would catch a hand-edit to either
   accessor's expression the moment it landed, before any query ever runs.
   This is the check this file's own module docstring commits to; run it
   with no arguments and no server.

2. LIVE (needs a running ClickHouse -- `--host`/`--port`/etc.): seeds one
   cpu_state row with refemu's own worked-example state (the first case in
   sqlcpu/test_checkpoint.py: pc=0x80000004, x1..x31 all zero -- known
   reghash 4903144380889844081, hex `440b77d621644971`), runs BOTH
   accessors against it, and asserts the reghash field each produces is
   byte-identical to each other AND to that already-established oracle
   value -- not just self-consistent, actually correct.

Usage:
    test_checkpoint_query.py                          # static check only
    test_checkpoint_query.py --host localhost --port 9000 --password clickdoom
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import checkpoint_query as cq  # noqa: E402

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sqlcpu"))
import checkpoint as cp  # noqa: E402

# refemu's worked example (issue #15; re-verified in sqlcpu/test_checkpoint.py):
# pc=0x80000004, x1..x31 all zero.
KNOWN_PC = 2147483652
KNOWN_REGS = [0] * 31
KNOWN_REGHASH = 4903144380889844081
KNOWN_REGHASH_HEX = format(KNOWN_REGHASH, "016x")


def check_static_equivalence() -> list[tuple[str, bool, str]]:
    """No ClickHouse needed: both accessors must embed the SAME
    checkpoint.reg_hash(pc="pc", regs="regs") call, not two hand-written
    copies that happen to look alike."""
    full_sql = cq.checkpoint_sql("clickdoom")
    reg_sql = cq.reg_checkpoint_sql("clickdoom")
    expr = cp.reg_hash(pc="pc", regs="regs")
    results = [
        (
            "checkpoint_sql() contains checkpoint.reg_hash(pc, regs) verbatim",
            expr in full_sql,
            "" if expr in full_sql else f"substring not found: {expr!r}",
        ),
        (
            "reg_checkpoint_sql() contains checkpoint.reg_hash(pc, regs) verbatim",
            expr in reg_sql,
            "" if expr in reg_sql else f"substring not found: {expr!r}",
        ),
        (
            "reg_checkpoint_sql() never computes ramhash/fbhash (SPEC §7: "
            "CHECKPOINT_INTERVAL cadence carries reghash only)",
            "ramhash" not in reg_sql and "fbhash" not in reg_sql,
            "" if "ramhash" not in reg_sql and "fbhash" not in reg_sql
            else "reg_checkpoint_sql() unexpectedly computes ramhash/fbhash -- "
                 "defeats the whole point of the cheap accessor (256x cost)",
        ),
    ]
    return results


def run_live_check(host, port, user, password, client, db) -> list[tuple[str, bool, str]]:
    """Seed one cpu_state row with the known-good state, run both
    accessors against a real server, and compare the reghash field each
    produces -- to each other, and to the independently-known-correct
    value. `checkpoint_sql()` also needs `ram`/`framebuffer`/`palette` to
    exist (even empty) -- applies the real sqlcpu/schema.sql rather than a
    second, driftable fixture copy of it (same convention
    executor/tests/test_commit.py already follows)."""
    client_cmd = client.split() + ["--host", host, "--port", str(port), "--user", user]
    if password:
        client_cmd += ["--password", password]

    def ch(query: str, fmt: str | None = None) -> subprocess.CompletedProcess:
        cmd = client_cmd + (["--format", fmt] if fmt else [])
        return subprocess.run(cmd, input=query, text=True, capture_output=True)

    schema_sql = (Path(__file__).resolve().parent.parent / "sqlcpu" / "schema.sql").read_text()
    schema_sql = schema_sql.replace("clickdoom.", f"{db}.")

    ch(f"DROP DATABASE IF EXISTS {db}")
    ch(f"CREATE DATABASE {db}")
    r = ch(schema_sql)
    if r.returncode != 0:
        return [("live check: apply sqlcpu/schema.sql", False, r.stderr.strip())]

    regs_literal = "[" + ",".join(str(r) for r in KNOWN_REGS) + "]"
    insert = (
        f"INSERT INTO {db}.cpu_state "
        "(batch_id, icount, pc, regs, halted, halt_reason, exit_code) "
        f"VALUES (1, 4096, {KNOWN_PC}, {regs_literal}, 0, '', 0)"
    )
    r = ch(insert)
    if r.returncode != 0:
        return [("live check: seed cpu_state fixture row", False, r.stderr.strip())]

    full_out = ch(cq.checkpoint_sql(db), fmt="TSVRaw")
    reg_out = ch(cq.reg_checkpoint_sql(db), fmt="TSVRaw")
    ch(f"DROP DATABASE IF EXISTS {db}")

    if full_out.returncode != 0:
        return [("live check: run checkpoint_sql()", False, full_out.stderr.strip())]
    if reg_out.returncode != 0:
        return [("live check: run reg_checkpoint_sql()", False, reg_out.stderr.strip())]

    full_fields = full_out.stdout.strip("\n").split("\t")
    reg_fields = reg_out.stdout.strip("\n").split("\t")
    full_reghash = full_fields[2] if len(full_fields) > 2 else "<missing>"
    reg_reghash = reg_fields[2] if len(reg_fields) > 2 else "<missing>"

    return [
        (
            "live: checkpoint_sql() reghash matches the known-correct oracle value",
            full_reghash == KNOWN_REGHASH_HEX,
            f"expected {KNOWN_REGHASH_HEX!r}, got {full_reghash!r}",
        ),
        (
            "live: reg_checkpoint_sql() reghash matches the known-correct oracle value",
            reg_reghash == KNOWN_REGHASH_HEX,
            f"expected {KNOWN_REGHASH_HEX!r}, got {reg_reghash!r}",
        ),
        (
            "live: both accessors produce byte-identical reghash for identical state",
            full_reghash == reg_reghash,
            f"checkpoint_sql()={full_reghash!r} reg_checkpoint_sql()={reg_reghash!r}",
        ),
    ]


def report(results: list[tuple[str, bool, str]]) -> int:
    fail = 0
    for name, ok, detail in results:
        if ok:
            print(f"  ok  -- {name}")
        else:
            fail += 1
            print(f"::error::FAILED -- {name}" + (f" ({detail})" if detail else ""), file=sys.stderr)
    return fail


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default=None, help="omit to run the static check only, no server needed")
    ap.add_argument("--port", default="9000")
    ap.add_argument("--user", default="default")
    ap.add_argument("--password", default="")
    ap.add_argument("--client", default="clickhouse-client")
    ap.add_argument("--database", default="clickdoom_test_checkpoint_query")
    args = ap.parse_args()

    print("# static equivalence check (no ClickHouse)")
    fail = report(check_static_equivalence())

    if args.host:
        print("# live equivalence check")
        fail += report(run_live_check(args.host, args.port, args.user, args.password,
                                       args.client, args.database))
    else:
        print("# (skipped live check -- pass --host to also run it against a real server)")

    if fail:
        print(f"test_checkpoint_query.py: {fail} check(s) FAILED", file=sys.stderr)
        return 1
    print("test_checkpoint_query.py: all checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
