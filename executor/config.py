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
# Measured, not guessed -- see docs/experiments/write-log-high-water-mark.md. 20,000 is the
# bottom of the measured per-step-cost curve for an all-store worst case.
#
# Re-confirmed against the real ROM by #257, which measured the realistic
# mixed-instruction case that RESULTS.md left open: the true optimum is
# ~17,960, worth 0.08%, so this value stands.
#
# But note what that measurement also found, because it is a live constraint
# rather than a safety valve: in the BOOT window general-RAM store density is
# 33.3%, so at the production K = 60,000 the write-log reaches 19,998 against
# this mark of 20,000. Boot runs about six instructions below early
# termination. A ROM change that raises store density even slightly will start
# truncating every boot batch, each truncation costing a full ~1.65 s batch
# setup -- which would read as a sudden ~6% throughput drop with no code change
# to blame it on.
WRITE_LOG_HIGH_WATER_MARK_DEFAULT = 20_000

# SPEC §3.1: instructions per emulated millisecond. Deferred/game-speed
# parameter per SPEC §9 -- not validated here, just plumbed through.
IPMS_DEFAULT = 10_000

# SPEC §2/§3: the MMIO window, and the five register offsets within it.
# Word access only; see fold.py's MMIO section for what non-register and
# non-word accesses do and why.
MMIO_BASE = 0x1000_0000
MMIO_SIZE = 4 * 1024
MMIO_TICKS_MS = 0x00
MMIO_KEYQ = 0x04
MMIO_EXIT = 0x08
MMIO_PUTCHAR = 0x0C
MMIO_FRAME_COMMIT = 0x10

# SPEC §2: FRAMEBUFFER and PALETTE. #130 -- write-only from fold.py's side
# (a load from either region halts BAD_ADDR by construction: fold.py's
# routing exemption from bad_addr_cond is gated on the access being a
# store, so the load path is simply never touched, per the team lead's
# framing on #130). Both sizes are exact multiples of 4 -- 64,000/4 =
# 16,000, 768/4 = 192 -- which is load-bearing for #130's word-only-store
# design: a word-aligned address inside [BASE, BASE+SIZE) can never spill
# past the region's end, so no separate boundary-overrun check is needed
# on top of the word-alignment one.
FRAMEBUFFER_BASE = 0x1100_0000
FRAMEBUFFER_SIZE = 64_000
PALETTE_BASE = 0x1101_0000
PALETTE_SIZE = 768

# SPEC §7.
CHECKPOINT_INTERVAL = 4_096
RAM_HASH_INTERVAL = 1_048_576

# SPEC §5: batch_commit retention, in batch_id lag (not wall-clock time --
# see ADR-0003's rejected-TTL writeup). "N=16 is generous headroom, not a
# tight bound" per SPEC's own text; only the latest row's bulky columns are
# ever read in normal operation.
BATCH_COMMIT_RETENTION_N = 16

SPEC_VERSION = "0.1.0"

# --- Fixture-only (this PR's local test schema, not SPEC) -------------------
RAM_BASE = 0x8000_0000
RAM_WORDS_DEFAULT = 6_291_456       # 24 MiB / 4, matches the baseline benchmark's fixture
TEXT_WORDS_DEFAULT = 524_288        # 2 MiB / 4, matches the baseline benchmark's fixture

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
# Not a SPEC §1 *fault* -- the ROM's own clean stop via SPEC §3's EXIT
# register. It travels through the same halted/halt_reason/exit_code columns
# as every fault (SPEC §5), so it needs a code here. SPEC §1's vocabulary now
# names it explicitly (#37), and "halted normally" is halt_reason = 'EXIT'
# with the written value in exit_code -- deliberately not an empty string,
# which a differential comparison could not tell from an unset column.
HALT_EXIT = 8

HALT_REASON_NAMES = {
    HALT_NONE: "",
    HALT_ILLEGAL_INSN: "ILLEGAL_INSN",
    HALT_SELF_MODIFY: "SELF_MODIFY",
    HALT_BAD_ADDR: "BAD_ADDR",
    HALT_MISALIGNED: "MISALIGNED",
    HALT_ECALL: "ECALL",
    HALT_EBREAK: "EBREAK",
    HALT_CSR: "CSR",
    HALT_EXIT: "EXIT",   # SPEC §1's vocabulary, per #37
}

# Collapsed op_id space. 0-27 match ADR-0002 exactly (fold_predecoded.py);
# 28-31 are new for #23's halt semantics, agreed with sqlcpu (PR #42/#46/#49).
OP_LOAD = 18
OP_STORE = 19
OP_ECALL = 28
OP_EBREAK = 29
OP_CSR = 30
OP_ILLEGAL = 31
