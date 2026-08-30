-- Local test schema for #23/#25, mirroring sqlcpu/schema.sql exactly, not
-- just SPEC §5's prose -- covers `ram`/`decoded`/`input_queue`, the tables
-- executor/tests/test_fold.py's select_only()-based cases actually touch.
-- `input_queue` was missing until #25 found it: PR #88's MMIO plumbing
-- added decode_with()'s KEYQT subquery (reads `{db}.input_queue`) to every
-- select_only()/batch() call, but never landed here, and
-- `make test-executor` isn't wired into CI (no `test-executor` job in
-- .github/workflows/ci.yml), so nothing ran select_only() against this
-- fixture between #88 landing and now to catch it -- every test_fold.py
-- case failed outright (UNKNOWN_TABLE) the first time this file's tests
-- were run again. Loud and immediate here, not the silent-wrong-answer
-- category this project usually has to watch for, but still worth fixing
-- in the same pass rather than leaving `test_fold.py` broken.
-- Column names (`id`/`tgt`/`mk`/`sg`, not SPEC §5's `op_id`/`target`/
-- `width_mask`/`sign_bit`) and the byte-address `pc` convention match
-- sqlcpu's real table exactly.
--
-- `state`/`batch_out` -- this fixture's former stand-in for "the previous
-- batch's committed state" before batch_commit was ratified -- are gone:
-- #25 landed the real thing (sqlcpu/schema.sql), test_fold.py never
-- referenced either table (it only exercises select_only(), never batch()),
-- and executor/tests/test_commit.py (batch()/commit.py's own tests) applies
-- the real sqlcpu/schema.sql directly rather than a second, driftable copy
-- of batch_commit/cpu_state/console_out's shape.
--
-- `word_addr` is absolute (byte_addr >> 2) per SPEC §5's parenthetical, not
-- relative to RAM_BASE, matching what sqlcpu's real table contains.

CREATE DATABASE IF NOT EXISTS clickdoom_executor;

DROP TABLE IF EXISTS clickdoom_executor.ram;
CREATE TABLE clickdoom_executor.ram
(
    word_addr UInt32,
    value     UInt32,
    version   UInt64
)
ENGINE = ReplacingMergeTree(version)
ORDER BY word_addr;

DROP TABLE IF EXISTS clickdoom_executor.decoded;
CREATE TABLE clickdoom_executor.decoded
(
    word_addr UInt32,
    id        UInt8,
    rd        UInt8,
    rs1       UInt8,
    rs2       UInt8,
    imm       UInt32,
    tgt       UInt32,
    mk        UInt32,
    sg        UInt8,
    raw       UInt32
)
ENGINE = MergeTree
ORDER BY word_addr;

DROP TABLE IF EXISTS clickdoom_executor.input_queue;
CREATE TABLE clickdoom_executor.input_queue
(
    event_seq UInt64,
    key_event UInt16,
    consumed  UInt8
)
ENGINE = MergeTree
ORDER BY event_seq;
