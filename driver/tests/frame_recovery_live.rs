//! What a run redoes when it resumes, against a real server.
//!
//! A batch that writes `FRAME_COMMIT` has two kinds of derivation: the four
//! flushes, and the `frames_out` readout. A process that dies between them
//! leaves a committed frame with no row, and the resume point is already
//! past the batch that committed it, so nothing revisits it: `cpu_state` is
//! consistent, every checkpoint passes, and the artefact simply has a hole.
//!
//! Each case seeds exactly the state such a death leaves behind, runs
//! [`clickdoom_driver::emulation::run::recover`] the way a resuming run
//! does, and checks what appears.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`). Behind the
//! `clickhouse-tests` feature, so a run without a server visibly excludes
//! it.

#![cfg(feature = "clickhouse-tests")]

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::emulation::run::recover;
use clickdoom_driver::render;
use clickdoom_driver::sql::split_statements;
use clickhouse::Row;
use serde::Serialize;

const SCHEMA_SQL: &str = include_str!("../../sqlcpu/schema.sql");

/// The batch each case commits its frame in, and the frame it commits.
/// Neither is 0 or 1, so a readout that ignored the batch it was given, or
/// wrote whatever row it found, would show up.
const BATCH_ID: u64 = 5;
const FRAME_NO: u32 = 42;
const COMMITTED_ICOUNT: u64 = 15_393_136;

/// Words written into each region. The readout zero-fills the rest, so a
/// handful is a genuinely sparse region and still a complete frame.
const FB_WORDS: &[(u32, u32)] = &[(0, 0x0403_0201), (7, 0xDEAD_BEEF), (15_999, 0x1122_3344)];
const PAL_WORDS: &[(u32, u32)] = &[(0, 0x00FF_8040), (191, 0x0102_0304)];

#[derive(Row, Serialize)]
struct WordRow {
    word_addr: u32,
    value: u32,
    version: u64,
}

fn conn_args(database: &str) -> ConnArgs {
    ConnArgs {
        host: std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
        port: std::env::var("CLICKHOUSE_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8123),
        user: "default".to_owned(),
        database: database.to_owned(),
        password: None,
    }
}

async fn provision(database: &str) -> Db {
    let admin = conn_args("default").connect();
    admin
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
    let qualified = SCHEMA_SQL
        .replace("clickdoom.", &format!("{database}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {database};"),
        );
    for statement in split_statements(&qualified) {
        admin.run(statement).await.expect("the schema applies");
    }
    conn_args(database).connect()
}

/// The state a process leaves when it dies after the flushes and before the
/// readout: the region tables hold the frame's pixels and `batch_commit`
/// records that the batch committed it, while `frames_out` is empty.
async fn seed_committed_frame(db: &Db, database: &str, batch_id: u64, has_frame: u8) {
    db.insert_all(
        "framebuffer",
        FB_WORDS.iter().map(|(word_addr, value)| WordRow {
            word_addr: *word_addr,
            value: *value,
            version: COMMITTED_ICOUNT,
        }),
    )
    .await
    .expect("the framebuffer words land");
    db.insert_all(
        "palette",
        PAL_WORDS.iter().map(|(word_addr, value)| WordRow {
            word_addr: *word_addr,
            value: *value,
            version: COMMITTED_ICOUNT,
        }),
    )
    .await
    .expect("the palette words land");
    db.run(&format!(
        "INSERT INTO {database}.batch_commit \
         (batch_id, icount, pc, regs, halted, halt_reason, exit_code, keyq_pos, \
          has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) \
         VALUES ({batch_id}, {COMMITTED_ICOUNT}, 0, [], 0, '', 0, 0, \
          {has_frame}, {FRAME_NO}, [], [], [], [])"
    ))
    .await
    .expect("the batch_commit row lands");
}

/// The dense region bytes the seeded words describe, which is what the
/// readout has to reconstruct.
fn dense(words: &[(u32, u32)], n_words: u32) -> Vec<u8> {
    let mut dense = vec![0u32; n_words as usize];
    for (word_addr, value) in words {
        dense[*word_addr as usize] = *value;
    }
    dense.iter().flat_map(|w| w.to_le_bytes()).collect()
}

async fn frames_out_rows(db: &Db, database: &str) -> Vec<(u32, u64)> {
    db.fetch_all(&format!(
        "SELECT frame_no, committed_icount FROM {database}.frames_out ORDER BY frame_no"
    ))
    .await
    .expect("frames_out reads back")
}

#[tokio::test]
async fn a_resume_writes_the_frame_the_crash_lost_and_never_a_second_one() {
    let database = format!("clickdoom_driver_framerec_{}", std::process::id());
    let db = provision(&database).await;
    seed_committed_frame(&db, &database, BATCH_ID, 1).await;

    assert!(
        frames_out_rows(&db, &database).await.is_empty(),
        "the seeded state is the one a crash before the readout leaves"
    );

    recover(&db, &database, BATCH_ID)
        .await
        .expect("the resume redoes the batch's derivations");
    assert_eq!(
        frames_out_rows(&db, &database).await,
        vec![(FRAME_NO, COMMITTED_ICOUNT)],
        "the lost frame did not come back on resume"
    );

    // The frame that came back is this frame, not an empty region.
    let expected = clickdoom_spec::fb_hash(
        &dense(FB_WORDS, render::FRAMEBUFFER_WORDS),
        &dense(PAL_WORDS, render::PALETTE_WORDS),
    );
    let actual: String = db
        .fetch_one(&render::frame_readout_fb_hash_sql(&database))
        .await
        .expect("the frame hash reads back");
    assert_eq!(actual, format!("{expected:016x}"));

    // A run resumes on every start, including one that lost nothing.
    recover(&db, &database, BATCH_ID)
        .await
        .expect("a second resume is safe");
    assert_eq!(
        frames_out_rows(&db, &database).await,
        vec![(FRAME_NO, COMMITTED_ICOUNT)],
        "redoing the readout wrote the frame twice"
    );

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}

#[tokio::test]
async fn a_resume_writes_no_frame_for_a_batch_that_committed_none() {
    let database = format!("clickdoom_driver_framerec_none_{}", std::process::id());
    let db = provision(&database).await;
    // Most batches commit no frame, so the unconditional redo has to be a
    // no-op for them rather than inventing a row from whatever the region
    // tables happen to hold.
    seed_committed_frame(&db, &database, BATCH_ID, 0).await;

    recover(&db, &database, BATCH_ID)
        .await
        .expect("the resume redoes the batch's derivations");
    assert!(
        frames_out_rows(&db, &database).await.is_empty(),
        "a batch that committed no frame produced a frames_out row"
    );

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}

#[tokio::test]
async fn a_resume_reads_out_the_batch_it_names_and_not_the_latest() {
    let database = format!("clickdoom_driver_framerec_named_{}", std::process::id());
    let db = provision(&database).await;
    // Two batches committed frames and only the older one's readout was
    // lost. A redo that read "the latest batch with a frame" would write
    // the wrong frame and leave the hole where it was.
    seed_committed_frame(&db, &database, BATCH_ID, 1).await;
    db.run(&format!(
        "INSERT INTO {database}.batch_commit \
         (batch_id, icount, pc, regs, halted, halt_reason, exit_code, keyq_pos, \
          has_frame, frame_no, wl_addr, wl_val, wl_icount, console_bytes) \
         VALUES ({}, {}, 0, [], 0, '', 0, 0, 1, {}, [], [], [], [])",
        BATCH_ID + 1,
        COMMITTED_ICOUNT + 1000,
        FRAME_NO + 1
    ))
    .await
    .expect("the later batch_commit row lands");

    recover(&db, &database, BATCH_ID)
        .await
        .expect("the resume redoes the named batch's derivations");
    assert_eq!(
        frames_out_rows(&db, &database).await,
        vec![(FRAME_NO, COMMITTED_ICOUNT)],
        "the redo read out a batch other than the one it was given"
    );

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}
