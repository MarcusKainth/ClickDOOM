//! `sqlcpu/fixtures/decode_vectors.tsv`: hand-encoded RV32IM instructions
//! with the `decoded` row each one must produce.
//!
//! The decode check compares `sqlcpu/decode.sql`'s output against these
//! rows. The execute check takes the same rows as already-decoded input and
//! runs each one through the fold, so both checks read this file rather
//! than each keeping its own copy of the encodings.

use super::harness::CheckError;

const VECTORS_TSV: &str = include_str!("../../../sqlcpu/fixtures/decode_vectors.tsv");

/// The columns are `word_addr`, the raw word, the fourteen decoded fields
/// the table stores, and a human label.
const COLUMNS: usize = 18;

pub struct DecodeVector {
    pub word_addr: u32,
    pub word: u32,
    pub id: u8,
    pub rd: u8,
    pub rs1: u8,
    pub rs2: u8,
    pub imm: u32,
    pub tgt: u32,
    pub mk: u32,
    pub sg: u8,
    pub m_sg1: u8,
    pub m_sg2: u8,
    pub m_hi: u8,
    pub d_sg: u8,
    pub cmp_sel: u8,
    pub neg: u8,
    pub tgt_mis: u8,
    pub note: String,
}

fn field<T: std::str::FromStr>(
    fields: &[&str],
    at: usize,
    line_no: usize,
) -> Result<T, CheckError> {
    fields[at].parse().map_err(|_| {
        CheckError::Mismatch(format!(
            "sqlcpu/fixtures/decode_vectors.tsv line {line_no}: column {} is {:?}, not a number",
            at + 1,
            fields[at]
        ))
    })
}

/// Every committed vector, in file order.
pub fn decode_vectors() -> Result<Vec<DecodeVector>, CheckError> {
    let mut vectors = Vec::new();
    for (index, line) in VECTORS_TSV.lines().enumerate() {
        let line_no = index + 1;
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != COLUMNS {
            return Err(CheckError::Mismatch(format!(
                "sqlcpu/fixtures/decode_vectors.tsv line {line_no}: {} columns, expected {COLUMNS}",
                fields.len()
            )));
        }
        vectors.push(DecodeVector {
            word_addr: field(&fields, 0, line_no)?,
            word: field(&fields, 1, line_no)?,
            id: field(&fields, 2, line_no)?,
            rd: field(&fields, 3, line_no)?,
            rs1: field(&fields, 4, line_no)?,
            rs2: field(&fields, 5, line_no)?,
            imm: field(&fields, 6, line_no)?,
            tgt: field(&fields, 7, line_no)?,
            mk: field(&fields, 8, line_no)?,
            sg: field(&fields, 9, line_no)?,
            m_sg1: field(&fields, 10, line_no)?,
            m_sg2: field(&fields, 11, line_no)?,
            m_hi: field(&fields, 12, line_no)?,
            d_sg: field(&fields, 13, line_no)?,
            cmp_sel: field(&fields, 14, line_no)?,
            neg: field(&fields, 15, line_no)?,
            tgt_mis: field(&fields, 16, line_no)?,
            note: fields[17].to_owned(),
        });
    }
    Ok(vectors)
}
