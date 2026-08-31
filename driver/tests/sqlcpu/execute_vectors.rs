//! One decoded instruction at a time, through the fold the DOOM run
//! executes, against an independent RV32I reference.
//!
//! Each vector is a one-instruction batch: its decoded row goes into
//! `decoded` at its own word, `select_only` folds a single step from that
//! word's pc with the vector's own register file, and the resulting pc,
//! register file, write-log and halt are compared against
//! [`super::oracle`]. Nothing here reaches into the fold's internals, so
//! every assertion runs through the same expression production uses.
//!
//! The committed decode vectors supply the instructions. Further groups
//! cover what those never reach: a misaligned branch, jal and jalr target,
//! the M-extension's zero and overflow operands, and x0's own rules.

use clickdoom_driver::client::Db;
use clickdoom_executor::config::{
    HALT_NONE, HALT_REASON_NAMES, OP_LOAD, OP_STORE, WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
};
use clickdoom_executor::fold::{SelectOnlyArgs, select_only};
use clickdoom_spec::RAM_BASE;
use clickhouse::Row;
use serde::Deserialize;

use super::harness::{self, CheckError, WordRow};
use super::oracle::{self, Input};
use super::vectors::decode_vectors;

/// Words of RAM the vectors run against. Large enough to hold both data
/// words well clear of the instruction words.
const RAM_WORDS: u32 = 1024;

/// The RAM word every load reads, and the one every store amends.
const LOAD_SLOT: u32 = 512;
const STORE_SLOT: u32 = 520;
const LOAD_WORD: u32 = 0xDEAD_BEEF;
const STORE_WORD: u32 = 0x1234_5678;

const LOAD: u8 = OP_LOAD as u8;
const STORE: u8 = OP_STORE as u8;

/// A jump target two bytes past a word boundary: 2-byte aligned, and not
/// the 4-byte alignment a jump needs.
const MISALIGNED_TARGET: u32 = RAM_BASE + 0x102;

/// ClickHouse refuses a query longer than `max_query_size` (262,144 bytes
/// by default), and a `SETTINGS` clause inside the query text cannot raise
/// the limit for parsing that same text. Every vector carries a full copy
/// of the fold, so how many fit in one query falls out of the fold's own
/// length rather than being a fixed count.
const QUERY_TEXT_BUDGET: usize = 240_000;

/// x1..x31 = 100..3100, so every register holds a distinct non-zero value
/// and a read of the wrong one is visible in the result.
fn base_regs() -> [u32; 31] {
    std::array::from_fn(|i| (i as u32 + 1) * 100)
}

/// x3 = 0 (a zero divisor), x4 = INT_MIN and x5 = -1 (the signed
/// division overflow), x6 = -100 (mulhsu's signed operand) and x7 a value
/// with the top bit set (mulhsu's unsigned operand, chosen so treating it
/// as signed flips the product's sign). The plain 100/200/300 sequence has
/// no zero, negative or top-bit-set value to reach these with.
const M_EXT_OVERRIDES: &[(u8, u32)] = &[
    (3, 0),
    (4, 0x8000_0000),
    (5, 0xFFFF_FFFF),
    (6, 0xFFFF_FF9C),
    (7, 0x8000_0001),
];

/// One decoded row plus the register file it runs against.
struct ExecVector {
    note: String,
    id: u8,
    rd: u8,
    rs1: u8,
    rs2: u8,
    imm: u32,
    tgt: u32,
    mk: u32,
    sg: u8,
    raw: u32,
    regs: [u32; 31],
}

impl ExecVector {
    fn new(note: &str, id: u8, rd: u8, rs1: u8, rs2: u8, imm: u32, tgt: u32) -> Self {
        ExecVector {
            note: note.to_owned(),
            id,
            rd,
            rs1,
            rs2,
            imm,
            tgt,
            mk: 0,
            sg: 0,
            raw: 0,
            regs: base_regs(),
        }
    }

    fn with_overrides(mut self, overrides: &[(u8, u32)]) -> Self {
        for (register, value) in overrides {
            self.regs[*register as usize - 1] = *value;
        }
        self
    }

    /// The RAM word this instruction's address lands in. Only a load or a
    /// store reads one.
    fn mem_word(&self) -> u32 {
        match self.id {
            LOAD => LOAD_WORD,
            STORE => STORE_WORD,
            _ => 0,
        }
    }
}

/// A misaligned branch, jal and jalr target, and an untaken branch whose
/// target would have been bad had it been taken. Every committed vector's
/// branch and jal offset is a multiple of 4, so none of them reach the
/// fault.
fn misaligned_vectors() -> Vec<ExecVector> {
    let bad = MISALIGNED_TARGET;
    vec![
        // jalr computes its own target from x1 (100) and the immediate,
        // so it needs no `tgt`: 102 has bit 1 set.
        ExecVector::new("jalr to a misaligned target", 27, 6, 1, 0, 2, 0),
        ExecVector::new("jal to a misaligned target", 26, 5, 0, 0, 0, bad),
        ExecVector::new("taken beq to a misaligned target", 20, 0, 1, 1, 0, bad),
        ExecVector::new("taken bne to a misaligned target", 21, 0, 1, 2, 0, bad),
        ExecVector::new(
            "untaken beq, whose bad target is never checked",
            20,
            0,
            1,
            2,
            0,
            bad,
        ),
    ]
}

/// The M-extension's zero and overflow operands, and mulhsu's
/// signed-by-unsigned asymmetry. The committed vectors' mul and div rows
/// all read x1 and x2 (100 and 200), neither zero nor negative.
fn m_extension_vectors() -> Vec<ExecVector> {
    let cases: &[(&str, u8, u8, u8)] = &[
        ("div by zero returns all-ones, no trap", 14, 1, 3),
        ("divu by zero returns all-ones, no trap", 15, 1, 3),
        ("rem by zero returns the dividend, no trap", 16, 1, 3),
        ("remu by zero returns the dividend, no trap", 17, 1, 3),
        ("div INT_MIN by -1 overflows to INT_MIN", 14, 4, 5),
        ("rem INT_MIN by -1 overflows to 0", 16, 4, 5),
        // The same bit pattern read unsigned. 0x80000000 divided by
        // 0xFFFFFFFF is ordinary unsigned division, with no overflow to
        // guard against.
        ("divu 0x80000000 by 0xFFFFFFFF is ordinary", 15, 4, 5),
        ("remu 0x80000000 by 0xFFFFFFFF is ordinary", 17, 4, 5),
        ("mulhsu multiplies signed rs1 by unsigned rs2", 12, 6, 7),
    ];
    cases
        .iter()
        .map(|(note, id, rs1, rs2)| {
            ExecVector::new(note, *id, 5, *rs1, *rs2, 0, 0).with_overrides(M_EXT_OVERRIDES)
        })
        .collect()
}

/// x0's own rules: it reads as zero whatever else is in the register file,
/// and a write to it never lands.
fn x0_vectors() -> Vec<ExecVector> {
    vec![
        ExecVector::new("x0 reads as zero", 0, 1, 0, 0, 0, 0),
        ExecVector::new("a write to x0 is discarded", 0, 0, 1, 2, 0, 0),
    ]
}

/// Every vector, with each load's and store's base register moved so its
/// address lands on this fixture's data word.
///
/// The committed vectors compute addresses from the plain 100/200/300
/// register sequence, which is below RAM and would fault. Moving the base
/// register keeps the instruction, the immediate and the byte offset
/// within the word exactly as committed, and only relocates where the
/// access lands.
fn all_vectors() -> Result<Vec<ExecVector>, CheckError> {
    let mut vectors: Vec<ExecVector> = decode_vectors()?
        .into_iter()
        .map(|vector| ExecVector {
            note: vector.note,
            id: vector.id,
            rd: vector.rd,
            rs1: vector.rs1,
            rs2: vector.rs2,
            imm: vector.imm,
            tgt: vector.tgt,
            mk: vector.mk,
            sg: vector.sg,
            raw: vector.word,
            regs: base_regs(),
        })
        .collect();
    vectors.extend(misaligned_vectors());
    vectors.extend(m_extension_vectors());
    vectors.extend(x0_vectors());

    for vector in &mut vectors {
        let slot = match vector.id {
            LOAD => LOAD_SLOT,
            STORE => STORE_SLOT,
            _ => continue,
        };
        if vector.rs1 == 0 {
            return Err(CheckError::Mismatch(format!(
                "vector {:?} reads its address from x0, which is always 0 and cannot be moved \
                 into RAM",
                vector.note
            )));
        }
        let byte_offset = vector.regs[vector.rs1 as usize - 1].wrapping_add(vector.imm) & 3;
        let landing = RAM_BASE + slot * 4 + byte_offset;
        vector.regs[vector.rs1 as usize - 1] = landing.wrapping_sub(vector.imm);
    }
    Ok(vectors)
}

/// The address a load or store computes, checked against what the fold's
/// memory model accepts. The reference treats every access as a plain RAM
/// access, so a vector whose access would fault is caught here, where the
/// message says which vector and why.
fn check_access(vector: &ExecVector) -> Result<(), CheckError> {
    if vector.id != LOAD && vector.id != STORE {
        return Ok(());
    }
    let a = if vector.rs1 == 0 {
        0
    } else {
        vector.regs[vector.rs1 as usize - 1]
    };
    let addr = a.wrapping_add(vector.imm);
    let end = RAM_BASE as u64 + RAM_WORDS as u64 * 4;
    if (addr as u64) < RAM_BASE as u64 || addr as u64 >= end {
        return Err(CheckError::Mismatch(format!(
            "vector {:?} addresses {addr:#010x}, outside the {RAM_WORDS}-word RAM this check \
             provisions",
            vector.note
        )));
    }
    let align_mask = match vector.mk {
        0xFFFF_FFFF => 3,
        0xFFFF => 1,
        _ => 0,
    };
    if addr & align_mask != 0 {
        return Err(CheckError::Mismatch(format!(
            "vector {:?} addresses {addr:#010x}, not aligned to its own width (mk={:#x})",
            vector.note, vector.mk
        )));
    }
    Ok(())
}

/// Every halt the vectors are there to reach.
const REACHED_HALTS: &[&str] = &["ECALL", "EBREAK", "CSR", "ILLEGAL_INSN", "MISALIGNED"];

/// What the vector set reaches, checked against what it is for. A set that
/// stored nothing, loaded nothing or halted nowhere would agree with a
/// reference that reached the same nothing, and report the same pass.
fn check_coverage(vectors: &[ExecVector], expected: &[oracle::Step]) -> Result<(), CheckError> {
    let mut missing = Vec::new();
    if !vectors.iter().any(|vector| vector.id == LOAD) {
        missing.push("no vector loads".to_owned());
    }
    if !expected.iter().any(|step| !step.wl_addr.is_empty()) {
        missing.push("no vector wrote to the write-log".to_owned());
    }
    if !expected.iter().any(|step| step.retired == 1) {
        missing.push("no vector retired".to_owned());
    }
    for halt in REACHED_HALTS {
        if !expected.iter().any(|step| step.halt_reason == *halt) {
            missing.push(format!("no vector halts with {halt}"));
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CheckError::Mismatch(format!(
            "the vector set no longer reaches what it is for: {}",
            missing.join("; ")
        )))
    }
}

fn halt_reason_name(code: u8) -> String {
    if code == HALT_NONE {
        return String::new();
    }
    HALT_REASON_NAMES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, name)| (*name).to_owned())
        .unwrap_or_else(|| format!("halt code {code}, which no name covers"))
}

/// One vector's fold result, tagged with the vector it belongs to so the
/// rows of a batched query can be matched back up.
#[derive(Row, Deserialize)]
struct VectorResult {
    vec: u32,
    pc: u32,
    regs: Vec<u32>,
    wl_addr: Vec<u32>,
    wl_val: Vec<u32>,
    halted: u8,
    halt_reason: u8,
    retired: u32,
}

/// `select_only` for one vector, wrapped so a batched query's rows carry
/// the vector's index.
fn vector_query(index: usize, vector: &ExecVector, database: &str, decn: u32) -> String {
    let regs: Vec<String> = vector
        .regs
        .iter()
        .map(|value| format!("toUInt32({value})"))
        .collect();
    let args = SelectOnlyArgs {
        pc0: Some(RAM_BASE + index as u32 * 4),
        regs0: Some(&regs),
        db: database,
        ..Default::default()
    };
    let fold = select_only(
        1,
        0,
        0,
        decn,
        RAM_WORDS,
        WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
        &args,
    );
    format!(
        "SELECT toUInt32({index}) AS vec, pc, regs, wl_addr, wl_val, halted, halt_reason, retired\n\
         FROM (\n{fold}\n)"
    )
}

const JOIN: &str = "\nUNION ALL\n";

/// Several vectors' folds as one query. The settings are the ones
/// `select_only` already sets on its own text, repeated at the outer level
/// because the AST-size limit is checked against the whole query and a
/// subquery's own `SETTINGS` clause does not raise it.
fn batch_sql(parts: &[String]) -> String {
    format!(
        "SELECT * FROM (\n{}\n)\nSETTINGS max_ast_elements = 500000, \
         max_expanded_ast_elements = 500000",
        parts.join(JOIN)
    )
}

/// The vectors packed into as few queries as `QUERY_TEXT_BUDGET` allows,
/// each a `UNION ALL` of the individual folds.
fn pack(queries: Vec<String>) -> Vec<Vec<String>> {
    let mut packed: Vec<Vec<String>> = Vec::new();
    let mut length = 0;
    for query in queries {
        let grows_to = length + query.len() + JOIN.len();
        match packed.last_mut() {
            Some(batch) if grows_to <= QUERY_TEXT_BUDGET => {
                length = grows_to;
                batch.push(query);
            }
            _ => {
                length = query.len();
                packed.push(vec![query]);
            }
        }
    }
    packed
}

pub async fn check(db: &Db, database: &str) -> Result<String, CheckError> {
    let vectors = all_vectors()?;
    for vector in &vectors {
        check_access(vector)?;
    }
    let decn = vectors.len() as u32;
    let ram_base_word = RAM_BASE / 4;

    harness::run(
        db,
        "clearing ram",
        &format!("TRUNCATE TABLE {database}.ram"),
    )
    .await?;
    harness::insert_all(
        db,
        "seeding the vectors' RAM",
        "ram",
        (0..RAM_WORDS).map(|word| WordRow {
            word_addr: ram_base_word + word,
            value: match word {
                LOAD_SLOT => LOAD_WORD,
                STORE_SLOT => STORE_WORD,
                _ => 0,
            },
            version: 0,
        }),
    )
    .await?;

    // The decoded rows come from the fixture rather than from decode.sql:
    // this check starts from an already-decoded instruction, which is what
    // the fold consumes. The decode check is what proves decode.sql
    // produces these rows.
    harness::run(
        db,
        "clearing decoded",
        &format!("TRUNCATE TABLE {database}.decoded"),
    )
    .await?;
    let rows: Vec<String> = vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| {
            format!(
                "({},{},{},{},{},{},{},{},{},{})",
                ram_base_word + index as u32,
                vector.id,
                vector.rd,
                vector.rs1,
                vector.rs2,
                vector.imm,
                vector.tgt,
                vector.mk,
                vector.sg,
                vector.raw
            )
        })
        .collect();
    harness::run(
        db,
        "loading the vectors' decoded rows",
        &format!(
            "INSERT INTO {database}.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) VALUES {}",
            rows.join(",")
        ),
    )
    .await?;

    let packed = pack(
        vectors
            .iter()
            .enumerate()
            .map(|(index, vector)| vector_query(index, vector, database, decn))
            .collect(),
    );
    let batches = packed.len();

    let mut results: Vec<Option<VectorResult>> = (0..vectors.len()).map(|_| None).collect();
    for batch in &packed {
        let rows: Vec<VectorResult> =
            harness::fetch_all(db, "folding a batch of execute vectors", &batch_sql(batch)).await?;
        for row in rows {
            let at = row.vec as usize;
            if at >= results.len() || results[at].is_some() {
                return Err(CheckError::Mismatch(format!(
                    "a batched fold returned vector index {at} twice, or out of range"
                )));
            }
            results[at] = Some(row);
        }
    }

    let expected: Vec<oracle::Step> = vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| {
            oracle::step(&Input {
                pc: RAM_BASE + index as u32 * 4,
                id: vector.id,
                rd: vector.rd,
                rs1: vector.rs1,
                rs2: vector.rs2,
                imm: vector.imm,
                tgt: vector.tgt,
                mk: vector.mk,
                sg: vector.sg,
                regs: &vector.regs,
                mem_word: vector.mem_word(),
                ram_base: RAM_BASE,
            })
        })
        .collect();
    check_coverage(&vectors, &expected)?;

    let mut mismatches = Vec::new();
    for (index, (vector, result)) in vectors.iter().zip(&results).enumerate() {
        let Some(result) = result else {
            return Err(CheckError::Mismatch(format!(
                "the batched folds returned no row for vector {index} ({:?})",
                vector.note
            )));
        };
        let pc = RAM_BASE + index as u32 * 4;
        let expected = &expected[index];
        let regs: [u32; 31] = result.regs.clone().try_into().map_err(|_| {
            CheckError::Mismatch(format!(
                "vector {index} came back with {} registers, and the file holds x1..x31",
                result.regs.len()
            ))
        })?;
        let actual = oracle::Step {
            pc: result.pc,
            regs,
            wl_addr: result.wl_addr.clone(),
            wl_val: result.wl_val.clone(),
            halted: result.halted,
            halt_reason: halt_reason_name(result.halt_reason),
            retired: result.retired,
        };
        if actual != *expected {
            mismatches.push(format!(
                "  vector {index} at pc={pc:#010x} ({}):\n    expected {expected:?}\n    \
                 actual   {actual:?}",
                vector.note
            ));
        }
    }
    if !mismatches.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "the fold disagrees with the reference on {} of {} vectors:\n{}",
            mismatches.len(),
            vectors.len(),
            mismatches.join("\n")
        )));
    }
    Ok(format!(
        "all {} vectors match the reference, across {batches} batched queries",
        vectors.len()
    ))
}
