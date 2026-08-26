"""Executor constants shared between the fold generator, fixtures, and tests.

Values that are part of the SPEC §6/§3.1/§7 contract (K, IPMS,
CHECKPOINT_INTERVAL, RAM_HASH_INTERVAL) are pinned here to the SPEC default.
Fixture-only values (RAM_WORDS, text bounds) describe the local test schema
this PR builds against (see executor/schema_fixture.sql) while
sqlcpu/schema.sql (#17) hasn't landed -- they are not contract values.
"""

# SPEC §6: instructions per batch.
K_DEFAULT = 50_000

# SPEC §6: batch ends early once the write-log reaches this many entries.
# Measured, not guessed -- see executor/bench/hwm/RESULTS.md. 20,000 is the
# bottom of the measured per-step-cost curve for an all-store worst case.
WRITE_LOG_HIGH_WATER_MARK_DEFAULT = 20_000

# SPEC §3.1: instructions per emulated millisecond. Deferred/game-speed
# parameter per SPEC §9 -- not validated here, just plumbed through.
IPMS_DEFAULT = 10_000

# SPEC §7.
CHECKPOINT_INTERVAL = 4_096
RAM_HASH_INTERVAL = 1_048_576

SPEC_VERSION = "0.1.0"

# --- Fixture-only (this PR's local test schema, not SPEC) -------------------
RAM_BASE = 0x8000_0000
RAM_WORDS_DEFAULT = 6_291_456       # 24 MiB / 4, matches Phase 0's fixture
TEXT_WORDS_DEFAULT = 524_288        # 2 MiB / 4, matches Phase 0's fixture

# Halt reason codes used inside the fold accumulator (mapped to the SPEC §5
# LowCardinality(String) `halt_reason` outside the fold, once per batch, not
# once per step -- keeps the reason-code arithmetic out of the hot lambda).
HALT_NONE = 0
HALT_ILLEGAL_INSN = 1
HALT_SELF_MODIFY = 2
HALT_BAD_ADDR = 3
HALT_MISALIGNED = 4
HALT_ECALL = 5
HALT_EBREAK = 6
HALT_CSR = 7

HALT_REASON_NAMES = {
    HALT_NONE: "",
    HALT_ILLEGAL_INSN: "ILLEGAL_INSN",
    HALT_SELF_MODIFY: "SELF_MODIFY",
    HALT_BAD_ADDR: "BAD_ADDR",
    HALT_MISALIGNED: "MISALIGNED",
    HALT_ECALL: "ECALL",
    HALT_EBREAK: "EBREAK",
    HALT_CSR: "CSR",
}

# Collapsed op_id space. 0-27 match ADR-0002 exactly (fold_predecoded.py);
# 28-31 are new for #23's halt semantics and are not yet agreed with sqlcpu.
OP_LOAD = 18
OP_STORE = 19
OP_ECALL = 28
OP_EBREAK = 29
OP_CSR = 30
OP_ILLEGAL = 31
