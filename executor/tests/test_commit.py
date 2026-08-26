"""Tests for #25's batch-commit flush (executor/commit.py) and the fold.py
`batch()` reshape that targets `batch_commit` directly.

Unlike test_fold.py (which builds its own schema_fixture.sql, since it
predates sqlcpu/schema.sql landing), this file applies the REAL
sqlcpu/schema.sql -- renamed onto an isolated private database, the same
technique executor/bench/batch_overhead/run.sh uses -- so batch_commit's
column shape can never drift from what sqlcpu actually ships. Every test
gets its own fresh database: cheap here (small fixtures), and it removes
any cross-test batch_id ordering assumptions.

Two things this file specifically validates, both because they are exactly
the kind of "silent, deterministic, wrong" bug this project keeps finding
late (#69, #81, #83, #101):

  1. `wl_icount` is the store's ABSOLUTE icount across batches, not merely
     distinct within one (#101) -- checked by writing to the same address
     from two batches with very different icount_base values and confirming
     `ram FINAL` reflects the chronologically later write, not whichever one
     merges last.
  2. Batch-commit atomicity survives a simulated crash between "the
     batch_commit row lands" and "the flush runs" -- checked using SPEC
     §7's own oracle (a RAM-region xxh64 hash, via sqlcpu/checkpoint.py's
     `word_array_hash`, not a hand-rolled comparison), per the plan posted
     on issue #25.

Requires a reachable ClickHouse: locally, `just up` (clickdoom-ch via
`docker exec` -- the default below, unchanged for local dev). In CI (#116),
`docker exec clickdoom-ch` doesn't exist -- see test_fold.py's matching
comment for the full reasoning. Set CLICKHOUSE_HOST to switch modes.

Run: cd executor && uv run pytest tests/test_commit.py -v
"""
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))       # executor/
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "sqlcpu"))  # sqlcpu/
import commit  # noqa: E402
import config  # noqa: E402
import fold  # noqa: E402
import checkpoint  # noqa: E402

CONTAINER = "clickdoom-ch"
DB = "clickdoom_executor_commit_test"
RAM_BASE = config.RAM_BASE
RAM_BASE_WORD = RAM_BASE >> 2
SCHEMA_SQL = Path(__file__).resolve().parents[2] / "sqlcpu" / "schema.sql"

# See test_fold.py's matching CH_HOST comment -- same switch, same reasoning.
CH_HOST = os.environ.get("CLICKHOUSE_HOST")


def ch(sql, fmt=None):
    if CH_HOST:
        cmd = ["clickhouse-client",
               "--host", CH_HOST,
               "--port", os.environ.get("CLICKHOUSE_PORT", "9000"),
               "--user", os.environ.get("CLICKHOUSE_USER", "default"),
               "--database", os.environ.get("CLICKHOUSE_DATABASE", "clickdoom"),
               "--multiquery"]
        if os.environ.get("CLICKHOUSE_PASSWORD"):
            cmd += ["--password", os.environ["CLICKHOUSE_PASSWORD"]]
    else:
        cmd = ["docker", "exec", "-i", CONTAINER, "clickhouse-client", "--multiquery"]
    if fmt:
        cmd += ["--format", fmt]
    r = subprocess.run(cmd, input=sql, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"clickhouse-client failed:\n{sql}\n{r.stderr}")
    return r.stdout


def scalar(sql):
    return ch(sql).strip()


@pytest.fixture(autouse=True)
def db():
    """Fresh, isolated database per test -- real sqlcpu/schema.sql, renamed
    (not a hand-copied approximation, same reasoning as
    executor/bench/batch_overhead/run.sh: this can't drift from what
    sqlcpu maintains, and won't collide with anything else touching the
    shared `clickdoom` database)."""
    ch(f"DROP DATABASE IF EXISTS {DB}")
    schema_text = SCHEMA_SQL.read_text()
    renamed = schema_text.replace("clickdoom.", f"{DB}.").replace(
        "CREATE DATABASE IF NOT EXISTS clickdoom;", f"CREATE DATABASE IF NOT EXISTS {DB};")
    ch(renamed)
    yield DB
    ch(f"DROP DATABASE IF EXISTS {DB}")


def seed_batch_commit(db, batch_id, pc, regs, icount, keyq_pos=0):
    """Directly seed a batch_commit row as the next batch's PREV -- bypasses
    bootstrap.py's CLI so tests can control pc/regs/icount precisely (e.g.
    a large icount_base to exercise #101), the same way test_fold.py builds
    decoded/ram rows directly rather than going through sqlcpu/decode.sql."""
    regs_sql = "[" + ",".join(str(r) for r in regs) + "]"
    ch(f"INSERT INTO {db}.batch_commit "
       f"(batch_id, icount, pc, regs, halted, halt_reason, exit_code, "
       f" keyq_pos, has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) "
       f"VALUES ({batch_id}, {icount}, {pc}, {regs_sql}, 0, '', 0, "
       f" {keyq_pos}, 0, 0, [], [], [], [])")


def seed_decoded_and_ram(db, decoded_rows, ram_words):
    """decoded_rows: list of (id, rd, rs1, rs2, imm, tgt, mk, sg, raw) at
    word_addr RAM_BASE_WORD+0, +1, .... `ram` is filled dense with zeros
    over [RAM_BASE, RAM_BASE + ram_words*4) -- the density invariant
    RAMT/decode positional indexing requires (#81), same as
    sqlcpu/load_rom.py."""
    ch(f"TRUNCATE TABLE {db}.decoded; TRUNCATE TABLE {db}.ram")
    rows = []
    for i, (id_, rd, rs1, rs2, imm, tgt, mk, sg, raw) in enumerate(decoded_rows):
        rows.append(f"({RAM_BASE_WORD + i},{id_},{rd},{rs1},{rs2},{imm},{tgt},{mk},{sg},{raw})")
    ch(f"INSERT INTO {db}.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) "
       f"VALUES {','.join(rows)}")
    ch(f"INSERT INTO {db}.ram (word_addr, value, version) "
       f"SELECT {RAM_BASE_WORD} + number, 0, 0 FROM numbers({ram_words})")


def run_batch(db, K, decn, ram_words, hwm=20_000):
    sql = fold.batch(K, 0, decn, decn, ram_words, hwm, db=db)
    ch(sql)


def flush_all(db):
    ch(commit.ram_flush_sql(db))
    ch(commit.console_out_flush_sql(db))
    ch(commit.cpu_state_flush_sql(db))


def ram_value_version(db, word_addr):
    out = json.loads(ch(
        f"SELECT value, version FROM {db}.ram FINAL WHERE word_addr = {word_addr} FORMAT JSONEachRow"
    ).strip())
    return int(out["value"]), int(out["version"])


def ram_hash(db, ram_words):
    """SPEC §7's ramhash, over just this test's small ram_words region --
    reuses sqlcpu/checkpoint.py's word_array_hash rather than a second,
    hand-rolled hashing scheme, so this test is validating against the same
    format the real differential trace uses."""
    words_expr = (f"(SELECT groupArray(value) FROM "
                  f"(SELECT value FROM {db}.ram FINAL "
                  f" WHERE word_addr >= {RAM_BASE_WORD} AND word_addr < {RAM_BASE_WORD + ram_words} "
                  f" ORDER BY word_addr))")
    return scalar(f"SELECT {checkpoint.hex64(checkpoint.word_array_hash(words_expr))}")


# addi rd, x0, imm  ->  op_id=0 (collapsed add arm), rs1=0, rs2=0
def ADDI(rd, imm):
    return (0, rd, 0, 0, imm, 0, 0xFFFFFFFF, 0, 0)


# sw rs2, imm(x0)  ->  op_id=19 (store), rs1=0 so address = imm directly
def SW(rs2, addr):
    return (config.OP_STORE, 0, 0, rs2, addr, 0, 0xFFFFFFFF, 0, 0)


def test_wl_icount_absolute_across_batches():
    """#101: wl_icount (and therefore ram.version) must be the store's
    absolute icount, not its rank within one batch. Two batches store to
    the SAME address; batch 2's icount_base is deliberately much larger
    than batch 1's, but its within-batch rank (1, since K=1) is smaller
    than batch 1's absolute icount. Pre-fix, both batches would compute
    wl_icount=1 -- an outright version TIE (unspecified ReplacingMergeTree
    winner, SPEC §8), not merely a wrong order. Post-fix, batch 2's version
    is icount_base + 1, strictly greater, so `ram FINAL` deterministically
    reflects batch 2's value."""
    decn, ram_words = 1, 9
    addr = RAM_BASE + decn * 4  # word offset `decn`: outside text, no SELF_MODIFY
    seed_decoded_and_ram(DB, [SW(rs2=1, addr=addr)], ram_words)

    # batch 1: icount_base=0, x1=VALUE1
    seed_batch_commit(DB, batch_id=0, pc=RAM_BASE, regs=[0x11111111] + [0] * 30, icount=0)
    run_batch(DB, K=1, decn=decn, ram_words=ram_words)
    flush_all(DB)
    word_addr = RAM_BASE_WORD + (addr - RAM_BASE) // 4
    v1, ver1 = ram_value_version(DB, word_addr)
    assert (v1, ver1) == (0x11111111, 1), "batch 1: value+version"

    # batch 2: icount_base=60000 (far larger), x1=VALUE2. Within-batch rank
    # is still 1 (K=1) -- the exact scenario #101 describes.
    seed_batch_commit(DB, batch_id=100, pc=RAM_BASE, regs=[0x22222222] + [0] * 30, icount=60_000)
    run_batch(DB, K=1, decn=decn, ram_words=ram_words)
    flush_all(DB)
    v2, ver2 = ram_value_version(DB, word_addr)
    assert ver2 == 60_001, "wl_icount must be icount_base + within-batch rank, not the rank alone"
    assert v2 == 0x22222222, "ram FINAL must reflect the chronologically later store"
    assert ver2 > ver1, "batch 2's version must strictly exceed batch 1's -- no tie"


def test_wl_icount_three_real_chained_batches_same_address():
    """Team lead's correction on #101's review: a two-batch same-address
    test can pass even under a double-counting bug (fold emits absolute
    wl_icount, but some flush site *also* adds icount_before on top) --
    doubling a monotonically-increasing icount_base still preserves
    relative order between batches chained through the real `PREV`
    mechanism, so a 2-batch test can't tell "correct" from "wrong but
    still-ordered" apart. Three REAL, naturally-chained batches (not
    manually-seeded out-of-order icounts, like the test above -- this one
    goes through fold.py's actual PREV/batch_commit progression, the same
    path production traffic uses) all storing to the SAME address is the
    shape that actually pins it: assert not just "the last write wins" but
    that every intermediate version is present and strictly increasing,
    which a uniform double-count would still show, but a wrong-source /
    stale-icount_before bug would not.
    """
    decn, ram_words = 6, 10
    addr = RAM_BASE + decn * 4  # outside text, no SELF_MODIFY
    values = [0xAAAAAAAA, 0xBBBBBBBB, 0xCCCCCCCC]
    decoded_rows = []
    for v in values:
        decoded_rows += [ADDI(rd=1, imm=v), SW(rs2=1, addr=addr)]
    seed_decoded_and_ram(DB, decoded_rows, ram_words)
    seed_batch_commit(DB, batch_id=0, pc=RAM_BASE, regs=[0] * 31, icount=0)

    word_addr = RAM_BASE_WORD + (addr - RAM_BASE) // 4
    versions = []
    for v in values:
        run_batch(DB, K=2, decn=decn, ram_words=ram_words)  # naturally chained via PREV
        flush_all(DB)
        val, ver = ram_value_version(DB, word_addr)
        assert val == v, f"ram FINAL must reflect the write just made ({v:#x}), got {val:#x}"
        versions.append(ver)

    assert versions == sorted(versions), f"versions must be strictly increasing in write order: {versions}"
    assert len(set(versions)) == 3, f"no two of the three writes may share a version: {versions}"
    final_val, final_ver = ram_value_version(DB, word_addr)
    assert final_val == values[-1], "after all three batches, ram FINAL must hold the LAST write"
    assert final_ver == versions[-1]


def test_crash_recovery_idempotent_flush():
    """Uses SPEC §7's own oracle (ramhash) to validate ADR-0003's atomicity
    claim: a batch_commit row can land with its flush skipped (simulating a
    crash before recovery), and redoing the flush later -- unconditionally,
    the same statement recovery always runs -- must converge to the exact
    same observable ram state as if the crash had never happened."""
    decn, ram_words = 6, 14
    A, B, C = (RAM_BASE + w * 4 for w in (decn, decn + 1, decn + 2))
    decoded_rows = [
        ADDI(rd=1, imm=0x11111111), SW(rs2=1, addr=A),
        ADDI(rd=2, imm=0x22222222), SW(rs2=2, addr=B),
        ADDI(rd=3, imm=0x33333333), SW(rs2=3, addr=C),
    ]

    def reset():
        # Both sub-runs share one database (the autouse `db` fixture only
        # resets between test *functions*), so batch_commit/cpu_state/
        # console_out from the first sub-run must be cleared before the
        # second seeds its own batch_id=0 -- otherwise the second sub-run's
        # PREV lookup (MAX(batch_id)) would keep picking up the first
        # sub-run's later, higher-numbered rows instead of its own.
        ch(f"TRUNCATE TABLE {DB}.batch_commit; TRUNCATE TABLE {DB}.cpu_state; "
           f"TRUNCATE TABLE {DB}.console_out")
        seed_decoded_and_ram(DB, decoded_rows, ram_words)

    def run_clean():
        reset()
        seed_batch_commit(DB, batch_id=0, pc=RAM_BASE, regs=[0] * 31, icount=0)
        for _ in range(3):  # 3 batches x K=2 == all 6 instructions
            run_batch(DB, K=2, decn=decn, ram_words=ram_words)
            flush_all(DB)
        return ram_hash(DB, ram_words)

    def run_with_simulated_crash():
        reset()
        seed_batch_commit(DB, batch_id=0, pc=RAM_BASE, regs=[0] * 31, icount=0)
        run_batch(DB, K=2, decn=decn, ram_words=ram_words)   # batch 1
        flush_all(DB)
        run_batch(DB, K=2, decn=decn, ram_words=ram_words)   # batch 2 -- crash before flush
        # (no flush_all here: the crash window)
        # recovery: unconditionally redo the flush for the latest batch_commit
        # row, exactly what the driver does on startup, before any new batch.
        flush_all(DB)
        run_batch(DB, K=2, decn=decn, ram_words=ram_words)   # batch 3, after recovery
        flush_all(DB)
        return ram_hash(DB, ram_words)

    clean_hash = run_clean()
    recovered_hash = run_with_simulated_crash()
    assert clean_hash == recovered_hash, (
        "a skipped-then-redone flush must converge to identical ram state -- "
        "any difference means a batch was observably half-applied, violating SPEC §6"
    )

    # And the redo itself must be a no-op on an ALREADY-flushed batch --
    # running flush_all a second time for the same latest batch_commit row
    # (no crash at all) must not change the hash either.
    hash_before_redo = ram_hash(DB, ram_words)
    flush_all(DB)
    assert ram_hash(DB, ram_words) == hash_before_redo, "redoing an already-applied flush must be a no-op"


def test_retention_delete_does_not_underflow_early_in_a_run():
    """commit.retention_sql's UInt64-underflow guard: on the very first
    batches of a run (batch_id well under N), a naive `max(batch_id) - N`
    computed in unsigned space wraps around to a huge value, and `batch_id <
    <huge>` would match (and delete) every row, including the one just
    committed. This must not happen."""
    seed_batch_commit(DB, batch_id=0, pc=RAM_BASE, regs=[0] * 31, icount=0)
    before = int(scalar(f"SELECT count() FROM {DB}.batch_commit"))
    assert before == 1
    ch(commit.retention_sql(DB, n=config.BATCH_COMMIT_RETENTION_N))
    after = int(scalar(f"SELECT count() FROM {DB}.batch_commit"))
    assert after == before, "retention must not delete anything when batch_id=0 is under the N=16 lag window"


def test_bootstrap_script_seeds_once_and_is_a_noop_on_replay():
    # This interpreter's own path, to run bootstrap.py as a subprocess and
    # test its CLI -- test-harness plumbing, not a computation delegated off
    # SQL. Named PY so the one purity-ok annotation covers every call site
    # below, rather than one per site.
    PY = sys.executable  # purity-ok: stdlib interpreter path, not a ClickHouse UDF
    bootstrap = str(Path(__file__).resolve().parents[1] / "bootstrap.py")
    # bootstrap.py appends --host/--port/--user/--database itself regardless
    # of --client's shape (see bootstrap.py: `args.client.split() +
    # ["--host", ...]`), so both forms below work unmodified -- only the
    # client program named differs, same CH_HOST switch as this file's ch().
    # --password is needed in network mode (the container requires one over
    # the wire, per docker-compose.yml/#3-#4) and harmless-but-unnecessary in
    # docker-exec mode (the container's own localhost access needs none).
    client = "clickhouse-client" if CH_HOST else "docker exec -i clickdoom-ch clickhouse-client"
    host = CH_HOST or "localhost"
    extra = ["--password", os.environ["CLICKHOUSE_PASSWORD"]] if CH_HOST and os.environ.get("CLICKHOUSE_PASSWORD") else []

    result = subprocess.run(
        [PY, bootstrap, "--host", host, "--port", os.environ.get("CLICKHOUSE_PORT", "9000"),
         "--database", DB, "--client", client] + extra,
        capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr
    row = json.loads(ch(
        f"SELECT pc, regs, icount, keyq_pos FROM {DB}.batch_commit WHERE batch_id = 0 FORMAT JSONEachRow"
    ).strip())
    assert int(row["pc"]) == RAM_BASE
    assert all(int(r) == 0 for r in row["regs"])
    assert int(row["icount"]) == 0

    # replay must be a no-op, not a second batch_id=0 row
    result2 = subprocess.run(
        [PY, bootstrap, "--host", host, "--port", os.environ.get("CLICKHOUSE_PORT", "9000"),
         "--database", DB, "--client", client] + extra,
        capture_output=True, text=True,
    )
    assert result2.returncode == 0, result2.stderr
    count = int(scalar(f"SELECT count() FROM {DB}.batch_commit WHERE batch_id = 0"))
    assert count == 1
