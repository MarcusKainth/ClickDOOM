//! The official rv32ui and rv32um suites, run to completion inside
//! ClickHouse on the fold the DOOM run executes.
//!
//! Each fixture's words go into `ram`, `sqlcpu/decode.sql` fills `decoded`
//! over them, and one `select_only` folds the whole test to its halt. A
//! fixture signals its result the way the suite does: an environment call,
//! with zero in `a0` for a pass and the failing case number encoded in it
//! otherwise.
//!
//! No read-only text region is declared, so a store into the image is an
//! ordinary write rather than a self-modify halt. The fixtures put their
//! data section inside the same image as their code and the reference
//! interpreter runs them the same way; declaring one would fail every
//! store test on the data it was told to write.
//!
//! These are the only checks on the SQL CPU that come from outside the
//! project.

use std::path::{Path, PathBuf};

use clickdoom_driver::client::Db;
use clickdoom_driver::decode;
use clickdoom_driver::fold_result::FoldResult;
use clickdoom_executor::config::{HALT_ECALL, WRITE_LOG_HIGH_WATER_MARK_DEFAULT};
use clickdoom_executor::fold::{SelectOnlyArgs, select_only};
use clickdoom_spec::RAM_BASE;

use super::harness::{self, CheckError, WordRow};

/// How many fixtures the builder produces. Checked before the loop below,
/// because a path typo turns that loop into a green run over nothing.
const EXPECTED_FIXTURES: usize = 48;

/// Words of RAM each fixture runs against: its image, then zeros. Every
/// fixture's code, data and every address it touches fit inside this, so a
/// bad-address halt means a real one.
const RAM_WORDS: u32 = 2048;

/// `arrayFold` evaluates every element of its range whatever the
/// accumulator says, so every fixture pays for the whole of this. The
/// longest one retires under a thousand instructions.
const MAX_INSTRUCTIONS: u32 = 4096;

/// A green run that retired almost nothing would report the same summary,
/// so the suite says how much actually executed.
const MIN_TOTAL_RETIRED: u64 = 10_000;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../refemu/tests/fixtures/riscv_tests")
}

fn fixtures() -> Result<Vec<PathBuf>, CheckError> {
    let dir = fixtures_dir();
    let entries = std::fs::read_dir(&dir).map_err(|source| CheckError::Read {
        path: dir.clone(),
        source,
    })?;
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CheckError::Read {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "bin") {
            found.push(path);
        }
    }
    found.sort();
    if found.len() != EXPECTED_FIXTURES {
        return Err(CheckError::Mismatch(format!(
            "{} holds {} fixtures, expected {EXPECTED_FIXTURES}. Did the directory move, or did \
             refemu/scripts/build_riscv_tests.sh not run?",
            dir.display(),
            found.len()
        )));
    }
    Ok(found)
}

/// A flat binary as little-endian words, zero-padded to a whole word.
fn words_of(path: &Path) -> Result<Vec<u32>, CheckError> {
    let mut bytes = std::fs::read(path).map_err(|source| CheckError::Read {
        path: path.to_owned(),
        source,
    })?;
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
    if bytes.len() > RAM_WORDS as usize * 4 {
        return Err(CheckError::Mismatch(format!(
            "{} is {} bytes, past the {RAM_WORDS}-word RAM this check provisions",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect())
}

/// How a fixture ended, in the suite's own terms.
fn verdict(result: &FoldResult) -> Result<u32, String> {
    if result.halted != 1 {
        return Err(format!(
            "did not halt within {MAX_INSTRUCTIONS} instructions (pc={:#010x})",
            result.pc
        ));
    }
    if result.halt_reason != HALT_ECALL {
        return Err(format!(
            "expected a clean ECALL exit, got halt code {} at pc={:#010x} (icount={})",
            result.halt_reason, result.halt_pc, result.retired
        ));
    }
    // regs holds x1..x31, so a0 (x10) is the tenth element.
    let a0 = result.regs.get(9).copied().unwrap_or_default();
    if a0 != 0 {
        return Err(format!(
            "case {} failed (a0={a0:#x}, icount={})",
            (a0 - 1) >> 1,
            result.retired
        ));
    }
    Ok(result.retired)
}

pub async fn check(db: &Db, database: &str) -> Result<String, CheckError> {
    let fixtures = fixtures()?;
    let ram_base_word = RAM_BASE / 4;
    let args = SelectOnlyArgs {
        pc0: Some(RAM_BASE),
        db: database,
        ..Default::default()
    };
    // Every fixture folds the same K over the same shape, so this text is
    // built once and reused: identical text is also what lets ClickHouse's
    // compiled-expression cache carry across fixtures.
    let fold = select_only(
        MAX_INSTRUCTIONS,
        0,
        0,
        RAM_WORDS,
        RAM_WORDS,
        WRITE_LOG_HIGH_WATER_MARK_DEFAULT,
        &args,
    );

    let mut failures = Vec::new();
    let mut retired_total = 0u64;
    for path in &fixtures {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let words = words_of(path)?;

        harness::run(
            db,
            &format!("clearing ram before {name}"),
            &format!("TRUNCATE TABLE {database}.ram"),
        )
        .await?;
        harness::insert_all(
            db,
            &format!("loading {name} into ram"),
            "ram",
            (0..RAM_WORDS).map(|word| WordRow {
                word_addr: ram_base_word + word,
                value: words.get(word as usize).copied().unwrap_or_default(),
                version: 0,
            }),
        )
        .await?;
        decode::decode(db, database, ram_base_word, ram_base_word + RAM_WORDS)
            .await
            .map_err(|source| CheckError::Query {
                what: format!("running sqlcpu/decode.sql over {name}"),
                first_line: format!("sqlcpu/decode.sql over {name}"),
                source,
            })?;

        let result: FoldResult = harness::fetch_one(db, &format!("folding {name}"), &fold).await?;
        match verdict(&result) {
            Ok(retired) => {
                retired_total += retired as u64;
                println!("  {name} ... ok ({retired} instructions)");
            }
            Err(detail) => {
                println!("  {name} ... FAILED ({detail})");
                failures.push(format!("  {name}: {detail}"));
            }
        }
    }

    if !failures.is_empty() {
        return Err(CheckError::Mismatch(format!(
            "{} of {} fixtures failed:\n{}",
            failures.len(),
            fixtures.len(),
            failures.join("\n")
        )));
    }
    if retired_total < MIN_TOTAL_RETIRED {
        return Err(CheckError::Mismatch(format!(
            "every fixture passed but the suite retired only {retired_total} instructions, under \
             the {MIN_TOTAL_RETIRED} a real run of it retires"
        )));
    }
    Ok(format!(
        "{}/{} fixtures passed, {retired_total} instructions retired",
        fixtures.len(),
        fixtures.len()
    ))
}
