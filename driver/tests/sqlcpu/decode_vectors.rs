//! `sqlcpu/decode.sql` against the committed decode vectors.
//!
//! The vectors' raw words go into `ram`, `decode.sql` fills `decoded` over
//! exactly their word range, and every stored field is compared against the
//! hand-verified one. This checks dispatch and field extraction against
//! known encodings; the riscv-tests check is what proves the decoded rows
//! then execute correctly.

use clickdoom_driver::client::Db;
use clickdoom_driver::emulation::decode;
use clickhouse::Row;
use serde::Deserialize;

use super::harness::{self, CheckError, WordRow};
use super::vectors::decode_vectors;

/// The fields `decoded` stores for a vector, in the order the fixture
/// carries them.
#[derive(Row, Deserialize, PartialEq, Eq, Debug)]
struct DecodedRow {
    word_addr: u32,
    id: u8,
    rd: u8,
    rs1: u8,
    rs2: u8,
    imm: u32,
    tgt: u32,
    mk: u32,
    sg: u8,
    m_sg1: u8,
    m_sg2: u8,
    m_hi: u8,
    d_sg: u8,
    cmp_sel: u8,
    neg: u8,
    tgt_mis: u8,
}

pub async fn check(db: &Db, database: &str) -> Result<String, CheckError> {
    let vectors = decode_vectors()?;
    let first = vectors
        .first()
        .ok_or_else(|| CheckError::Mismatch("the decode vector file is empty".to_owned()))?
        .word_addr;
    let last = vectors[vectors.len() - 1].word_addr;

    harness::run(
        db,
        "clearing ram",
        &format!("TRUNCATE TABLE {database}.ram"),
    )
    .await?;
    harness::insert_all(
        db,
        "loading the decode vectors into ram",
        "ram",
        vectors.iter().map(|vector| WordRow {
            word_addr: vector.word_addr,
            value: vector.word,
            version: 0,
        }),
    )
    .await?;

    decode::decode(db, database, first, last + 1)
        .await
        .map_err(|source| CheckError::Query {
            what: "running sqlcpu/decode.sql over the decode vectors".to_owned(),
            first_line: format!("sqlcpu/decode.sql over [{first}, {})", last + 1),
            source,
        })?;

    let actual: Vec<DecodedRow> = harness::fetch_all(
        db,
        "reading decoded back",
        &format!(
            "SELECT word_addr, id, rd, rs1, rs2, imm, tgt, mk, sg, \
             m_sg1, m_sg2, m_hi, d_sg, cmp_sel, neg, tgt_mis \
             FROM {database}.decoded ORDER BY word_addr"
        ),
    )
    .await?;

    if actual.len() != vectors.len() {
        return Err(CheckError::Mismatch(format!(
            "decode.sql produced {} rows over [{first}, {}), expected {}",
            actual.len(),
            last + 1,
            vectors.len()
        )));
    }

    let mut mismatches = Vec::new();
    for (vector, actual) in vectors.iter().zip(&actual) {
        let expected = DecodedRow {
            word_addr: vector.word_addr,
            id: vector.id,
            rd: vector.rd,
            rs1: vector.rs1,
            rs2: vector.rs2,
            imm: vector.imm,
            tgt: vector.tgt,
            mk: vector.mk,
            sg: vector.sg,
            m_sg1: vector.m_sg1,
            m_sg2: vector.m_sg2,
            m_hi: vector.m_hi,
            d_sg: vector.d_sg,
            cmp_sel: vector.cmp_sel,
            neg: vector.neg,
            tgt_mis: vector.tgt_mis,
        };
        if *actual != expected {
            mismatches.push(format!(
                "  word_addr={} ({}):\n    expected {expected:?}\n    actual   {actual:?}",
                vector.word_addr, vector.note
            ));
        }
    }
    if !mismatches.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "sqlcpu/decode.sql disagrees with sqlcpu/fixtures/decode_vectors.tsv on {} of {} \
             vectors:\n{}",
            mismatches.len(),
            vectors.len(),
            mismatches.join("\n")
        )));
    }
    Ok(format!("all {} vectors match", vectors.len()))
}
