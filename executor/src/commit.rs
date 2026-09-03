//! Idempotent batch-commit flush and retention: the derivations that turn
//! `batch_commit` (the batch's single atomic write, produced by
//! [`crate::fold::batch`]) into `ram`/`framebuffer`/`palette`/
//! `console_out`/`cpu_state`'s observable state, plus the retention
//! statement that keeps `batch_commit` itself bounded.
//!
//! Every statement here names the batch it acts on. A caller flushing the
//! batch it just ran passes that batch's id; a caller recovering at startup
//! reads `max(batch_id)` once and passes that, so "run this after every
//! batch" and "run this unconditionally on startup, before any new batch,
//! to recover from a crash" stay the same statement with a different
//! argument. Each is safe to run any number of times for the same batch,
//! since every target table dedups on the flushed row's own natural key.

use clickdoom_spec::RAM_BASE;

/// Flushes `batch_id`'s write-log into `ram`. `wl_addr` is RAM_BASE-
/// relative; `ram.word_addr` is absolute. Adding `RAM_BASE >> 2` back on is
/// load-bearing: getting this wrong is silent, deterministic corruption in
/// a positionally indexed `ram` array, not an error anywhere.
pub fn ram_flush_sql(db: &str, batch_id: u64) -> String {
    let ram_base_word = RAM_BASE >> 2;
    format!(
        "INSERT INTO {db}.ram (word_addr, value, version)\n\
         SELECT {ram_base_word} + t.1, t.2, t.3\n\
         FROM (\n    \
             SELECT arrayJoin(arrayZip(wl_addr, wl_val, wl_icount)) AS t\n    \
             FROM {db}.batch_commit\n    \
             WHERE batch_id = {batch_id}\n\
         )"
    )
}

/// Flushes `batch_id`'s FRAMEBUFFER/PALETTE write-logs into `framebuffer`/
/// `palette`. Unlike [`ram_flush_sql`], no `RAM_BASE`-style rebasing:
/// `fb_wl_addr`/`pal_wl_addr` are already relative to each region's own
/// base, matching `framebuffer`/`palette.word_addr`'s own convention. One
/// statement per region, since they are two separate tables with two
/// separate source array-triples.
pub fn fbpal_flush_sql(db: &str, batch_id: u64) -> String {
    format!(
        "INSERT INTO {db}.framebuffer (word_addr, value, version)\n\
         SELECT t.1, t.2, t.3\n\
         FROM (\n    \
             SELECT arrayJoin(arrayZip(fb_wl_addr, fb_wl_val, fb_wl_icount)) AS t\n    \
             FROM {db}.batch_commit\n    \
             WHERE batch_id = {batch_id}\n\
         );\n\
         INSERT INTO {db}.palette (word_addr, value, version)\n\
         SELECT t.1, t.2, t.3\n\
         FROM (\n    \
             SELECT arrayJoin(arrayZip(pal_wl_addr, pal_wl_val, pal_wl_icount)) AS t\n    \
             FROM {db}.batch_commit\n    \
             WHERE batch_id = {batch_id}\n\
         )"
    )
}

/// Flushes `batch_id`'s `console_bytes` into `console_out`. `seq` packs
/// `batch_id` into the high 32 bits and the byte's array position into the
/// low bits: collision-proof by construction, since no batch's console
/// output can approach 2**32 bytes (bounded by K).
pub fn console_out_flush_sql(db: &str, batch_id: u64) -> String {
    format!(
        "INSERT INTO {db}.console_out (seq, byte)\n\
         SELECT bitShiftLeft(bc.batch_id, 32) + (t.1 - 1), t.2\n\
         FROM (\n    \
             SELECT batch_id, arrayJoin(arrayZip(arrayEnumerate(console_bytes), console_bytes)) AS t\n    \
             FROM {db}.batch_commit\n    \
             WHERE batch_id = {batch_id}\n\
         ) AS bc"
    )
}

/// Flushes `batch_id`'s `cpu_state` row: a pure projection of
/// `batch_commit`'s matching seven columns, no unnesting, the cheapest of
/// the flushes.
pub fn cpu_state_flush_sql(db: &str, batch_id: u64) -> String {
    format!(
        "INSERT INTO {db}.cpu_state (batch_id, icount, pc, regs, halted, halt_reason, exit_code)\n\
         SELECT batch_id, icount, pc, regs, halted, halt_reason, exit_code\n\
         FROM {db}.batch_commit\n\
         WHERE batch_id = {batch_id}"
    )
}

/// Drops whole `batch_commit` rows older than `batch_id - n`, batch-id lag
/// rather than wall-clock time. The threshold is still arithmetic the
/// server evaluates; what the caller supplies is which batch the window
/// trails. The signed-arithmetic detour guards a real `UInt64` underflow:
/// on any of the first `n` batches of a run, `batch_id - n` computed
/// directly in `UInt64` space wraps around to a huge value instead of going
/// negative, which would match (and delete) every row in the table,
/// including the batch just committed. Flooring at 0 in `Int64` before
/// casting back avoids the wraparound.
pub fn retention_sql(db: &str, batch_id: u64, n: u32) -> String {
    format!(
        "DELETE FROM {db}.batch_commit\n\
         WHERE batch_id < toUInt64(greatest(toInt64(0), toInt64({batch_id}) - {n}))\n\
         SETTINGS lightweight_deletes_sync = 0"
    )
}

/// [`retention_sql`]'s default retention window, in batch_id lag.
pub const RETENTION_N_DEFAULT: u32 = crate::config::BATCH_COMMIT_RETENTION_N;
