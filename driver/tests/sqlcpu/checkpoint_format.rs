//! The checkpoint expressions, evaluated by a real server.
//!
//! Each hash is checked against `clickdoom-spec`'s own implementation of
//! the same function, so this is a live differential between the two
//! engines rather than a third copy of the pinned values. The hex and line
//! formats are checked against literals, since the format is what they
//! are.

use clickdoom_driver::checkpoint::{
    fb_hash, format_checkpoint, hex32, hex64, reg_hash, word_array_hash,
};
use clickdoom_driver::client::Db;
use clickhouse::Row;
use serde::Deserialize;

use super::harness::{self, CheckError};

#[derive(Row, Deserialize)]
struct CheckRow {
    name: String,
    ok: u8,
}

struct Check {
    name: &'static str,
    /// The SQL expression under test, and the literal it must equal.
    expr: String,
    expected: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn regs_sql(regs: &[u32; 32]) -> String {
    let values: Vec<String> = regs[1..32].iter().map(u32::to_string).collect();
    format!("[{}]", values.join(","))
}

fn checks() -> Vec<Check> {
    // reg_hash over the reset register file, and over one with the top,
    // bottom and a middle register set.
    let zero_regs = [0u32; 32];
    let zero_pc = 0x8000_0004;
    let mut mixed_regs = [0u32; 32];
    mixed_regs[1] = 0xDEAD_BEEF;
    mixed_regs[10] = 42;
    mixed_regs[31] = 0xFFFF_FFFF;
    let mixed_pc = 0x8000_0100;

    // Sixteen words whose little-endian serialization is exactly the bytes
    // 0x00 through 0x3f.
    let ram: Vec<u8> = (0..64u8).collect();
    let words: Vec<String> = ram
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]).to_string())
        .collect();

    let framebuffer: Vec<u8> = (0..16u8).collect();
    let palette: Vec<u8> = (200..208u8).collect();

    vec![
        Check {
            name: "reg_hash over the reset register file",
            expr: reg_hash(&format!("toUInt32({zero_pc})"), &regs_sql(&zero_regs)),
            expected: clickdoom_spec::reg_hash(zero_pc, &zero_regs).to_string(),
        },
        Check {
            name: "reg_hash with x1, x10 and x31 set",
            expr: reg_hash(&format!("toUInt32({mixed_pc})"), &regs_sql(&mixed_regs)),
            expected: clickdoom_spec::reg_hash(mixed_pc, &mixed_regs).to_string(),
        },
        Check {
            name: "word_array_hash over sixteen words",
            expr: word_array_hash(&format!("[{}]", words.join(","))),
            expected: clickdoom_spec::ram_hash(&ram).to_string(),
        },
        Check {
            name: "fb_hash over a framebuffer then a palette",
            expr: fb_hash(
                &format!("unhex('{}')", hex(&framebuffer)),
                &format!("unhex('{}')", hex(&palette)),
            ),
            expected: clickdoom_spec::fb_hash(&framebuffer, &palette).to_string(),
        },
        Check {
            name: "hex64 pads to sixteen digits",
            expr: hex64("toUInt64(255)"),
            expected: "'00000000000000ff'".to_owned(),
        },
        Check {
            name: "hex32 pads to eight digits",
            expr: hex32("toUInt32(255)"),
            expected: "'000000ff'".to_owned(),
        },
        Check {
            name: "format_checkpoint without the memory hashes",
            expr: format_checkpoint(
                "toUInt64(4096)",
                "toUInt32(2147487744)",
                "toUInt64(1311768467463790320)",
                None,
            ),
            expected: "'4096\\t80001000\\t123456789abcdef0'".to_owned(),
        },
        Check {
            name: "format_checkpoint with the memory hashes",
            expr: format_checkpoint(
                "toUInt64(1048576)",
                "toUInt32(2147483648)",
                "toUInt64(0)",
                Some(("toUInt64(1)", "toUInt64(2)")),
            ),
            expected:
                "'1048576\\t80000000\\t0000000000000000\\t0000000000000001\\t0000000000000002'"
                    .to_owned(),
        },
    ]
}

pub async fn check(db: &Db) -> Result<String, CheckError> {
    let checks = checks();
    let parts: Vec<String> = checks
        .iter()
        .map(|check| {
            format!(
                "SELECT '{}' AS name, toUInt8(({}) = {}) AS ok",
                check.name, check.expr, check.expected
            )
        })
        .collect();
    let rows: Vec<CheckRow> = harness::fetch_all(
        db,
        "evaluating the checkpoint expressions",
        &parts.join("\nUNION ALL\n"),
    )
    .await?;

    if rows.len() != checks.len() {
        return Err(CheckError::Mismatch(format!(
            "{} of {} checkpoint checks returned a row",
            rows.len(),
            checks.len()
        )));
    }
    let failed: Vec<&str> = rows
        .iter()
        .filter(|row| row.ok != 1)
        .map(|row| row.name.as_str())
        .collect();
    if !failed.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "the checkpoint SQL disagrees with clickdoom-spec on: {}",
            failed.join(", ")
        )));
    }
    Ok(format!("all {} checks passed", checks.len()))
}
