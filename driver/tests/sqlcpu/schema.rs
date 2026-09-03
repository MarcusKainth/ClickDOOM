//! `sqlcpu/schema.sql`'s own DDL, against the database it just created.
//!
//! Every table declared there has to exist with the engine it names and
//! accept a round-trip row with `spec_version` defaulted. This catches a
//! DDL typo, a wrong engine and a wrong `ORDER BY` before any instruction
//! executes, none of which a fold reads back.
//!
//! The rows are removed again, so the checks that follow start from empty
//! tables.

use clickdoom_driver::client::Db;

use super::harness::{self, CheckError};

/// Each table and the engine `sqlcpu/schema.sql` gives it. An engine is
/// not a column, so `preflight`'s schema gate does not reach it.
const ENGINES: &[(&str, &str)] = &[
    ("cpu_state", "ReplacingMergeTree"),
    ("batch_commit", "MergeTree"),
    ("ram", "ReplacingMergeTree"),
    ("framebuffer", "ReplacingMergeTree"),
    ("palette", "ReplacingMergeTree"),
    ("input_queue", "MergeTree"),
    ("frames_out", "MergeTree"),
    ("console_out", "ReplacingMergeTree"),
    ("decoded", "MergeTree"),
];

/// One row per table, naming only the columns without a default.
const ROUND_TRIP: &[(&str, &str, &str)] = &[
    (
        "cpu_state",
        "batch_id, icount, pc, regs, halted, halt_reason, exit_code",
        "(0, 0, 2147483648, [], 0, '', 0)",
    ),
    (
        "batch_commit",
        "batch_id, icount, pc, regs, halted, halt_reason, exit_code, keyq_pos, has_frame, \
         frame_no, wl_addr, wl_val, wl_icount, console_bytes",
        "(0, 0, 2147483648, [], 0, '', 0, 0, 0, 0, [], [], [], [])",
    ),
    ("ram", "word_addr, value, version", "(0, 0, 0)"),
    ("framebuffer", "word_addr, value, version", "(0, 0, 0)"),
    ("palette", "word_addr, value, version", "(0, 0, 0)"),
    ("input_queue", "event_seq, key_event, consumed", "(0, 0, 0)"),
    (
        "frames_out",
        "frame_no, committed_icount, fb, palette",
        "(0, 0, '', '')",
    ),
    ("console_out", "seq, byte", "(0, 0)"),
    (
        "decoded",
        "word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg, raw",
        "(0, 0, 0, 0, 0, 0, 0, 0, 0, 0)",
    ),
];

/// The version every table's `spec_version` column defaults to.
const SPEC_VERSION: &str = "0.3.0";

pub async fn check(db: &Db, database: &str) -> Result<String, CheckError> {
    let mut wrong = Vec::new();
    for (table, expected) in ENGINES {
        let engine: String = harness::fetch_one(
            db,
            &format!("reading {table}'s engine"),
            &format!(
                "SELECT engine FROM system.tables WHERE database = '{database}' AND name = \
                 '{table}'"
            ),
        )
        .await?;
        if engine != *expected {
            wrong.push(format!(
                "  {table} has engine {engine}, expected {expected}"
            ));
        }
    }
    if !wrong.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "sqlcpu/schema.sql's tables do not have the engines it declares:\n{}",
            wrong.join("\n")
        )));
    }

    for (table, columns, values) in ROUND_TRIP {
        harness::run(
            db,
            &format!("round-tripping a row through {table}"),
            &format!("INSERT INTO {database}.{table} ({columns}) VALUES {values}"),
        )
        .await?;
        let version: String = harness::fetch_one(
            db,
            &format!("reading {table}'s spec_version back"),
            &format!("SELECT spec_version FROM {database}.{table} LIMIT 1"),
        )
        .await?;
        if version != SPEC_VERSION {
            wrong.push(format!(
                "  {table}.spec_version defaulted to {version:?}, expected {SPEC_VERSION:?}"
            ));
        }
        harness::run(
            db,
            &format!("clearing {table} after its round-trip"),
            &format!("TRUNCATE TABLE {database}.{table}"),
        )
        .await?;
    }
    if !wrong.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "sqlcpu/schema.sql's spec_version default did not survive a round trip:\n{}",
            wrong.join("\n")
        )));
    }

    Ok(format!(
        "{} tables have the engine they declare and round-trip a row",
        ENGINES.len()
    ))
}
