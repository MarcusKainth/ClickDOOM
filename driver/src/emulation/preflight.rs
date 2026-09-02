//! Five gates against a multi-hour run: refuses to start rather than
//! advising, since advice gets skipped at hour zero by someone in a hurry.
//!
//! 1. `decoded` is populated and the right size. An empty or short
//!    `decoded` does not error, it silently executes no-ops and reports
//!    flattering throughput.
//! 2. `ram` is dense over the RAM region. `load-rom` asserts this at load
//!    time already; this re-checks rather than trusts provisioning order.
//! 3. The loaded ROM binary matches `rom/PINNED_HASH` exactly.
//! 4. A real, isolated smoke-test batch actually retires what it's asked
//!    to, run against a throwaway database seeded with the same loaded ROM
//!    state, and the SELF_MODIFY guard fires on a synthetic self-modifying
//!    store, before the real run is trusted to go unattended for hours.
//! 5. The database's live schema (table/column/type shape) matches
//!    `sqlcpu/schema.sql`. `CREATE TABLE IF NOT EXISTS` never retrofits a
//!    column onto an existing table, so a database created before a schema
//!    change silently stays on the old shape forever.

use std::path::Path;

use clickdoom_executor::fold::{SelectOnlyArgs, select_only};
use clickdoom_spec::{Manifest, RAM_BASE, sha256_hex};

use crate::client::{ConnArgs, Db, Error};
use crate::emulation::fold_result::FoldResult;
use crate::emulation::rom::RAM_WORDS_DEFAULT;
use crate::sql::split_statements;

pub(crate) const SCHEMA_SQL: &str = include_str!("../../../sqlcpu/schema.sql");
pub(crate) const PINNED_HASH: &str = include_str!("../../../rom/PINNED_HASH");

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("gate 1 (decoded density): {0}")]
    Decoded(String),
    #[error("gate 2 (ram density): {0}")]
    Ram(String),
    #[error("gate 3 (ROM hash): {0}")]
    RomHash(String),
    #[error("gate 4 (smoke test): {0}")]
    Smoke(String),
    #[error("gate 5 (schema): {0}")]
    Schema(String),
    #[error("reading {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Manifest(#[from] clickdoom_spec::manifest::ManifestError),
    #[error(transparent)]
    Db(#[from] Error),
}

/// What passing every gate found, so the caller can report it.
pub struct Provenance {
    pub rom_sha256: String,
    pub decoded_rows: u64,
    pub k: u32,
    pub hwm: u32,
    pub database: String,
}

/// Runs every gate against `database`, in order, stopping at the first
/// failure. `bin`/`manifest_path` name the ROM the run is about to use;
/// `k`/`hwm` are the run's own batch parameters (the smoke test uses
/// `min(k, 1000)`).
pub async fn check(
    db: &Db,
    conn: &ConnArgs,
    bin: &Path,
    manifest_path: &Path,
    k: u32,
    hwm: u32,
) -> Result<Provenance, GateError> {
    let manifest = Manifest::read(manifest_path)?;
    let text_start = manifest.text_start.unwrap_or(RAM_BASE);
    let text_end = manifest.text_end.unwrap_or(RAM_BASE);
    let text_start_word = text_start / 4;
    let text_end_word = text_end / 4;
    let ram_base_word = RAM_BASE / 4;
    let ram_words = RAM_WORDS_DEFAULT;

    // Gate 1: decoded populated and correctly sized. count(DISTINCT
    // word_addr), not count(): decoded is plain MergeTree, no per-key
    // dedup, so a duplicate row at one address and a genuine gap at
    // another can cancel out in a bare count(). min/max close the
    // remaining gap: count(DISTINCT) equal to the expected width with
    // matching endpoints is airtight by pigeonhole.
    let expected_decoded = (text_end_word - text_start_word) as u64;
    let (dec_cnt, dec_min, dec_max): (u64, u64, u64) = db
        .fetch_one(
            "SELECT count(DISTINCT word_addr), toUInt64(min(word_addr)), toUInt64(max(word_addr)) FROM decoded",
        )
        .await?;
    if dec_cnt == 0 {
        return Err(GateError::Decoded(
            "decoded is EMPTY (0 rows): the run would execute K no-ops per batch, retire nothing real, and report throughput that looks fine. Run decode before retrying."
                .to_string(),
        ));
    }
    if dec_cnt != expected_decoded
        || dec_min != text_start_word as u64
        || dec_max != (text_end_word - 1) as u64
    {
        return Err(GateError::Decoded(format!(
            "decoded is not dense over [text_start_word={text_start_word}, text_end_word={text_end_word}): got count(DISTINCT)={dec_cnt} min={dec_min} max={dec_max}, expected count={expected_decoded} min={text_start_word} max={}",
            text_end_word - 1
        )));
    }

    // Gate 2: ram dense over the RAM region. count(), not count(DISTINCT):
    // ram is ReplacingMergeTree and this reads it FINAL, which guarantees
    // at most one row per word_addr, so the two forms are provably the
    // same number here.
    let (ram_cnt, ram_min, ram_max): (u64, u64, u64) = db
        .fetch_one(
            "SELECT count(), toUInt64(min(word_addr)), toUInt64(max(word_addr)) FROM ram FINAL",
        )
        .await?;
    if ram_cnt != ram_words as u64
        || ram_min != ram_base_word as u64
        || ram_max != (ram_base_word + ram_words - 1) as u64
    {
        return Err(GateError::Ram(format!(
            "ram is not dense over the RAM region: got count={ram_cnt} min={ram_min} max={ram_max}, expected count={ram_words} min={ram_base_word} max={}. RAM is indexed positionally: a sparse ram silently displaces every load past the first gap, no halt, no error.",
            ram_base_word + ram_words - 1
        )));
    }

    // Gate 3: the loaded ROM binary matches rom/PINNED_HASH exactly.
    let blob = std::fs::read(bin).map_err(|source| GateError::Read {
        path: bin.to_owned(),
        source,
    })?;
    let actual = sha256_hex(&blob);
    let pinned = PINNED_HASH.trim();
    if actual != pinned {
        return Err(GateError::RomHash(format!(
            "{}: sha256 ({actual}) != rom/PINNED_HASH ({pinned}). A run against the wrong binary produces a divergence hours in that looks like a CPU bug, not a stale artifact.",
            bin.display()
        )));
    }
    if let Some(manifest_sha) = &manifest.sha256
        && &actual != manifest_sha
    {
        return Err(GateError::RomHash(format!(
            "{}: sha256 ({actual}) != {}'s own sha256 field ({manifest_sha}): the binary and its manifest were built at different times and don't belong together, even though the binary happens to match PINNED_HASH.",
            bin.display(),
            manifest_path.display()
        )));
    }
    // Gate 4: a real smoke-test batch, run against a throwaway database
    // seeded from the CURRENT ram/decoded state via a table-to-table copy
    // (no re-decoding, no re-loading): this proves the current state
    // actually works, not a fresh one. Calls select_only() directly, the
    // same code path the real run's batches execute, not a hand-rolled
    // query.
    let smoke_db = format!("clickdoom_preflight_smoke_{}", std::process::id());
    let smoke_k = k.min(1000);
    db.run(&format!("DROP DATABASE IF EXISTS {smoke_db}"))
        .await?;
    db.run(&format!("CREATE DATABASE {smoke_db}")).await?;
    for table in ["ram", "decoded", "input_queue"] {
        db.run(&format!(
            "CREATE TABLE {smoke_db}.{table} AS {}.{table}",
            conn.database
        ))
        .await?;
        db.run(&format!(
            "INSERT INTO {smoke_db}.{table} SELECT * FROM {}.{table}",
            conn.database
        ))
        .await?;
    }
    // RAM-relative, not absolute: build_step() compares text_start_widx/
    // text_end_widx directly against WA, which is always RAM_BASE-relative.
    let text_start_widx = text_start_word - ram_base_word;
    let text_end_widx = text_end_word - ram_base_word;
    let smoke_args = SelectOnlyArgs {
        pc0: Some(RAM_BASE),
        db: &smoke_db,
        ..Default::default()
    };
    let smoke_sql = select_only(
        smoke_k,
        text_start_widx,
        text_end_widx,
        expected_decoded as u32,
        ram_words,
        hwm,
        &smoke_args,
    );
    let smoke: FoldResult = db.fetch_one(&smoke_sql).await?;
    let smoke_result = if smoke.halted == 0 && smoke.retired != smoke_k {
        Err(GateError::Smoke(format!(
            "smoke-test batch (K={smoke_k}) retired {}, not {smoke_k}, and did not halt: arrayFold ran all {smoke_k} steps regardless, so a working run and a stalled one look identical by wall clock alone.",
            smoke.retired
        )))
    } else {
        Ok(())
    };
    db.run(&format!("DROP DATABASE IF EXISTS {smoke_db}"))
        .await?;
    smoke_result?;

    // Gate 4 (continued): the SELF_MODIFY guard actually fires. The
    // retirement check above cannot catch a unit-confusion bug in
    // text_start_widx/text_end_widx, since the real loaded ROM's boot
    // slice never performs a genuine self-modifying store within smoke_k
    // instructions -- "SELF_MODIFY never fires" looks identical whether the
    // arm is wired correctly or silently dead. A synthetic self-modifying
    // store, seeded into its own tiny throwaway database, proves the
    // mechanism itself fires end to end.
    if text_start_widx != 0 {
        return Err(GateError::Smoke(format!(
            "text_start_widx={text_start_widx}, expected 0: the SELF_MODIFY synthetic check targets word 0 relative to RAM_BASE and needs text_start_widx=0 for that to land inside the text window."
        )));
    }
    let selfmod_db = format!("clickdoom_preflight_selfmod_{}", std::process::id());
    let sm_decn = 8u32;
    let sm_ram_words = 8u32;
    db.run(&format!("DROP DATABASE IF EXISTS {selfmod_db}"))
        .await?;
    db.run(&format!("CREATE DATABASE {selfmod_db}")).await?;
    for table in ["ram", "decoded", "input_queue"] {
        db.run(&format!(
            "CREATE TABLE {selfmod_db}.{table} AS {}.{table}",
            conn.database
        ))
        .await?;
    }
    // word 0 (relative to RAM_BASE): id=OP_STORE, mk=full word, sg=0,
    // imm=RAM_BASE -- store the word at RAM_BASE, i.e. at itself. Words
    // 1..7 are OP_ILLEGAL padding, never reached: K=1 halts on the very
    // first step if the guard fires correctly.
    let mut selfmod_rows = format!(
        "({ram_base_word},{},0,0,0,{RAM_BASE},0,4294967295,0,0)",
        clickdoom_executor::config::OP_STORE
    );
    for i in 1..sm_decn {
        selfmod_rows.push_str(&format!(
            ",({},{},0,0,0,0,0,0,0,{})",
            ram_base_word + i,
            clickdoom_executor::config::OP_ILLEGAL,
            0xBAD00000u32 + i
        ));
    }
    db.run(&format!(
        "INSERT INTO {selfmod_db}.decoded (word_addr,id,rd,rs1,rs2,imm,tgt,mk,sg,raw) VALUES {selfmod_rows}"
    ))
    .await?;
    db.run(&format!(
        "INSERT INTO {selfmod_db}.ram (word_addr,value,version) SELECT {ram_base_word} + number, 0, 0 FROM numbers({sm_ram_words})"
    ))
    .await?;
    let selfmod_args = SelectOnlyArgs {
        pc0: Some(RAM_BASE),
        db: &selfmod_db,
        ..Default::default()
    };
    let selfmod_sql = select_only(1, 0, sm_decn, sm_decn, sm_ram_words, hwm, &selfmod_args);
    let selfmod: FoldResult = db.fetch_one(&selfmod_sql).await?;
    let selfmod_result = if selfmod.halted != 1
        || selfmod.halt_reason != clickdoom_executor::config::HALT_SELF_MODIFY
    {
        Err(GateError::Smoke(format!(
            "SELF_MODIFY guard did not fire on a synthetic self-modifying store (got halted={} halt_reason={}, expected halted=1 halt_reason={}): the SELF_MODIFY mechanism itself is not working.",
            selfmod.halted,
            selfmod.halt_reason,
            clickdoom_executor::config::HALT_SELF_MODIFY
        )))
    } else {
        Ok(())
    };
    db.run(&format!("DROP DATABASE IF EXISTS {selfmod_db}"))
        .await?;
    selfmod_result?;

    // Gate 5: the database's live schema matches sqlcpu/schema.sql,
    // checked by building a throwaway reference database from the CURRENT
    // schema.sql and comparing system.columns -- this comparison can never
    // drift from what schema.sql actually declares, unlike a hand-
    // maintained expected-column list.
    let schema_ref_db = format!("clickdoom_preflight_schema_ref_{}", std::process::id());
    db.run(&format!("DROP DATABASE IF EXISTS {schema_ref_db}"))
        .await?;
    let qualified_schema = SCHEMA_SQL
        .replace("clickdoom.", &format!("{schema_ref_db}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {schema_ref_db};"),
        );
    for statement in split_statements(&qualified_schema) {
        db.run(statement).await?;
    }
    let cols_query = |database: &str| {
        format!(
            "SELECT table, name, type FROM system.columns WHERE database = '{database}' ORDER BY table, position"
        )
    };
    let ref_cols: Vec<(String, String, String)> = db.fetch_all(&cols_query(&schema_ref_db)).await?;
    let live_cols: Vec<(String, String, String)> =
        db.fetch_all(&cols_query(&conn.database)).await?;
    let schema_result = if ref_cols != live_cols {
        Err(GateError::Schema(format!(
            "{}'s live schema (table, column, type) does not match sqlcpu/schema.sql. For any table/column missing from the live side: if empty, drop and recreate the database from schema.sql; if it holds data, ALTER TABLE ... ADD COLUMN instead (CREATE TABLE IF NOT EXISTS never retrofits an existing table). For anything present only on the live side, or a differing type, investigate before running.",
            conn.database
        )))
    } else {
        Ok(())
    };
    db.run(&format!("DROP DATABASE IF EXISTS {schema_ref_db}"))
        .await?;
    schema_result?;

    Ok(Provenance {
        rom_sha256: actual,
        decoded_rows: dec_cnt,
        k,
        hwm,
        database: conn.database.clone(),
    })
}
