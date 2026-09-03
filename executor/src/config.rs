//! Constants the fold and commit SQL text is generated against. Values
//! `clickdoom-spec` already carries (RAM base/size, MMIO layout, checkpoint
//! intervals) are read from there rather than re-declared.

use clickdoom_spec::RAM_SIZE;

/// Instructions per batch.
pub const K_DEFAULT: u32 = 50_000;

/// A batch ends early once the write-log reaches this many entries. The
/// bottom of the measured per-step-cost curve for an all-store worst case
/// (`docs/experiments/write-log-high-water-mark.md`).
pub const WRITE_LOG_HIGH_WATER_MARK_DEFAULT: u32 = 20_000;

/// `batch_commit` retention, in batch_id lag.
pub const BATCH_COMMIT_RETENTION_N: u32 = 16;

/// Bytes of a query ClickHouse keeps in `system.query_log.query`. A longer
/// one is stored as a prefix, with no error and no marker, so a log-based
/// reconstruction of what ran degrades without saying so.
///
/// This is the server's own default and nothing under `docker/clickhouse/`
/// overrides it, so the value is pinned here and checked against a live
/// server rather than assumed.
pub const LOG_QUERIES_CUT_TO_LENGTH: usize = 100_000;

/// Word count of the RAM region a dense `ram` spans.
pub const RAM_WORDS_DEFAULT: u32 = RAM_SIZE / 4;

/// Word count of the CLI default's text region, a generic fixture bound
/// rather than a real ROM's actual text size.
pub const TEXT_WORDS_DEFAULT: u32 = 524_288;

/// Halt reason codes used inside the fold accumulator. Mapped to the SPEC
/// `LowCardinality(String)` `halt_reason` outside the fold, once per batch,
/// not once per step, via [`HALT_REASON_NAMES`].
pub const HALT_NONE: u8 = 0;
pub const HALT_ILLEGAL_INSN: u8 = 1;
pub const HALT_SELF_MODIFY: u8 = 2;
pub const HALT_BAD_ADDR: u8 = 3;
pub const HALT_MISALIGNED: u8 = 4;
pub const HALT_ECALL: u8 = 5;
pub const HALT_EBREAK: u8 = 6;
pub const HALT_CSR: u8 = 7;
/// Not a fault: the ROM's own clean stop via the EXIT register. It travels
/// through the same halted/halt_reason/exit_code columns as every fault, so
/// it needs a code here. "Halted normally" is `halt_reason = 'EXIT'` with
/// the written value in `exit_code`, deliberately not an empty string,
/// which a differential comparison could not tell from an unset column.
pub const HALT_EXIT: u8 = 8;

/// `(code, name)` pairs, in the order `_halt_reason_transform`'s SQL
/// `transform()` call lists them. `HALT_NONE` is not among them: an
/// unrecognized code falls through to `transform`'s own `''` default,
/// which is `HALT_NONE`'s name anyway.
pub const HALT_REASON_NAMES: &[(u8, &str)] = &[
    (HALT_ILLEGAL_INSN, "ILLEGAL_INSN"),
    (HALT_SELF_MODIFY, "SELF_MODIFY"),
    (HALT_BAD_ADDR, "BAD_ADDR"),
    (HALT_MISALIGNED, "MISALIGNED"),
    (HALT_ECALL, "ECALL"),
    (HALT_EBREAK, "EBREAK"),
    (HALT_CSR, "CSR"),
    (HALT_EXIT, "EXIT"),
];

/// Collapsed op_id space. 0-27 match the decode table's own numbering;
/// 28-31 are the fatal-halt decode arms.
pub const OP_LOAD: u32 = 18;
pub const OP_STORE: u32 = 19;
pub const OP_ECALL: u32 = 28;
pub const OP_EBREAK: u32 = 29;
pub const OP_CSR: u32 = 30;
pub const OP_ILLEGAL: u32 = 31;
