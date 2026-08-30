//! Live proof for `render.rs`, run against a real ClickHouse server.
//!
//! `render_golden.rs` only proves the generated SQL text is byte-identical
//! to a known-correct reference; it never executes a query. This runs every
//! render function for real and checks the bytes it actually produces:
//!
//!   * a genuinely sparse `framebuffer`/`palette` table (only some
//!     `word_addr` rows present) reconstructs as a zero-filled dense region,
//!     not a shorter, misaligned one -- a bare `groupArray`/`FINAL` read,
//!     included here as a negative control, is shown to get this wrong on
//!     the same data;
//!   * `frame_readout_sql()` against a real `refemu`-captured frame
//!     reproduces the exact `fb_hash` that capture recorded;
//!   * `ppm_render_sql()` on that same frame byte-matches an independent
//!     re-derivation from the frame's own raw pixels and palette;
//!   * `ansi_render_sql()` and `ppm_render_sql()` byte-match a hand-computed
//!     escape sequence and a hand-computed PPM on a small synthetic frame.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`/`clickdoom`) and a
//! built `refemu` (`REFEMU`, default `target/release/refemu`) against the
//! pinned ROM.

use std::path::{Path, PathBuf};
use std::process::Command;

use bytes::Bytes;
use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::render::{self, FB_HEIGHT, FB_WIDTH, FRAMEBUFFER_WORDS, PALETTE_WORDS};
use clickdoom_driver::sql::split_statements;
use clickhouse::Row;
use refemu::snapshot::{Kind, Snapshot};
use serde::Serialize;

const FIXTURE_SCHEMA: &str = include_str!("../fixture_schema.sql");
const TARGET_ICOUNT: u64 = 15_393_136;
const EXPECTED_FBHASH: &str = "fe5d82c0f42d45f1";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn conn_args() -> ConnArgs {
    ConnArgs {
        host: std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
        port: std::env::var("CLICKHOUSE_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8123),
        user: "default".to_owned(),
        database: "default".to_owned(),
        password: None,
    }
}

fn db_at(conn: &ConnArgs, database: &str) -> Db {
    let mut c = conn.clone();
    c.database = database.to_owned();
    c.connect()
}

#[derive(Row, Serialize)]
struct WordRow {
    word_addr: u32,
    value: u32,
    version: u64,
}

async fn setup_fixture_db(conn: &ConnArgs, testdb: &str) -> Db {
    let admin = db_at(conn, "default");
    admin
        .run(&format!("DROP DATABASE IF EXISTS {testdb}"))
        .await
        .unwrap();
    let schema = FIXTURE_SCHEMA.replace("{{DB}}", testdb);
    for statement in split_statements(&schema) {
        admin.run(statement).await.unwrap();
    }
    db_at(conn, testdb)
}

async fn truncate_fixture_tables(db: &Db) {
    for table in ["framebuffer", "palette", "batch_commit", "frames_out"] {
        db.run(&format!("TRUNCATE TABLE {table}")).await.unwrap();
    }
}

async fn insert_batch_commit_row(db: &Db, database: &str, icount: u64, frame_no: u32) {
    db.run(&format!(
        "INSERT INTO {database}.batch_commit \
         (batch_id, icount, pc, regs, halted, halt_reason, exit_code, keyq_pos, has_frame, frame_no, \
          wl_addr, wl_val, wl_icount, console_bytes) \
         VALUES (1, {icount}, 0, [], 0, '', 0, 0, 1, {frame_no}, [], [], [], [])"
    ))
    .await
    .unwrap();
}

/// A bare `groupArray(value)`/`FINAL` read over `framebuffer`/`palette`,
/// with no dense zero-fill over an unwritten address: included as a
/// negative control, since a genuinely sparse region shortens this read
/// instead of zero-filling it, shifting every later byte.
fn old_readout_sql(db: &str) -> String {
    let old_fb_words = format!(
        "(SELECT groupArray(value) FROM (SELECT value FROM {db}.framebuffer FINAL ORDER BY word_addr))"
    );
    let old_pal_words = format!(
        "(SELECT groupArray(value) FROM (SELECT value FROM {db}.palette FINAL ORDER BY word_addr))"
    );
    let fb_bytes = render::region_bytes_sql(&old_fb_words);
    let pal_bytes = render::region_bytes_sql(&old_pal_words);
    format!(
        "INSERT INTO {db}.frames_out (frame_no, committed_icount, fb, palette)\n\
         SELECT frame_no, icount, {fb_bytes} AS fb, {pal_bytes} AS palette\n\
         FROM (\n    \
             SELECT frame_no, icount\n    \
             FROM {db}.batch_commit\n    \
             WHERE has_frame = 1\n    \
             ORDER BY batch_id DESC\n    \
             LIMIT 1\n\
         )"
    )
}

/// A deterministic, non-zero pseudo-value per address: a fixed
/// multiplicative hash (Knuth's constant), so the fixture is the same on
/// every run.
fn word_value(addr: u32) -> u32 {
    ((addr as u64 + 1).wrapping_mul(2_654_435_761)) as u32
}

fn le_words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

async fn read_fbhash(db: &Db, database: &str) -> String {
    let sql = render::frame_readout_fb_hash_sql(database);
    db.fetch_one(&sql).await.unwrap()
}

/// `frame_readout_sql()` must reconstruct a genuinely sparse
/// `framebuffer`/`palette` region as zero-filled, not shortened. Runs the
/// bare-groupArray query on the same seed data first, as a negative
/// control -- if it happened to produce the right hash anyway, the case
/// would not be exercising sparseness at all.
async fn run_sparse_case(conn: &ConnArgs, testdb: &str, which: &str, written: u32) {
    let db = db_at(conn, testdb);
    truncate_fixture_tables(&db).await;

    let dense_fb: Vec<u32> = (0..FRAMEBUFFER_WORDS).map(word_value).collect();
    let dense_pal: Vec<u32> = (0..PALETTE_WORDS).map(word_value).collect();

    let (fb_rows, pal_rows, expected_fb, expected_pal) = if which == "fb" {
        let fb_rows: Vec<WordRow> = (0..written)
            .map(|a| WordRow {
                word_addr: a,
                value: dense_fb[a as usize],
                version: 1,
            })
            .collect();
        let pal_rows: Vec<WordRow> = (0..PALETTE_WORDS)
            .map(|a| WordRow {
                word_addr: a,
                value: dense_pal[a as usize],
                version: 1,
            })
            .collect();
        let expected_fb: Vec<u32> = (0..FRAMEBUFFER_WORDS)
            .map(|a| if a < written { dense_fb[a as usize] } else { 0 })
            .collect();
        (
            fb_rows,
            pal_rows,
            le_words(&expected_fb),
            le_words(&dense_pal),
        )
    } else {
        let fb_rows: Vec<WordRow> = (0..FRAMEBUFFER_WORDS)
            .map(|a| WordRow {
                word_addr: a,
                value: dense_fb[a as usize],
                version: 1,
            })
            .collect();
        let pal_rows: Vec<WordRow> = (0..written)
            .map(|a| WordRow {
                word_addr: a,
                value: dense_pal[a as usize],
                version: 1,
            })
            .collect();
        let expected_pal: Vec<u32> = (0..PALETTE_WORDS)
            .map(|a| {
                if a < written {
                    dense_pal[a as usize]
                } else {
                    0
                }
            })
            .collect();
        (
            fb_rows,
            pal_rows,
            le_words(&dense_fb),
            le_words(&expected_pal),
        )
    };
    let expected_fbhash = format!(
        "{:016x}",
        clickdoom_spec::fb_hash(&expected_fb, &expected_pal)
    );

    db.insert_all("framebuffer", fb_rows.into_iter())
        .await
        .unwrap();
    db.insert_all("palette", pal_rows.into_iter())
        .await
        .unwrap();
    insert_batch_commit_row(&db, testdb, 1, 1).await;

    db.run(&old_readout_sql(testdb)).await.unwrap();
    let old_fbhash = read_fbhash(&db, testdb).await;
    assert_ne!(
        old_fbhash, expected_fbhash,
        "sparse-{which} negative control did not fail: the bare-groupArray query produced the \
         expected hash anyway, so this case does not exercise sparseness"
    );

    db.run(&format!("TRUNCATE TABLE {testdb}.frames_out"))
        .await
        .unwrap();
    db.run(&render::frame_readout_sql(testdb)).await.unwrap();
    let new_fbhash = read_fbhash(&db, testdb).await;
    assert_eq!(
        new_fbhash, expected_fbhash,
        "sparse-{which}: fixed frame_readout_sql() did not reconstruct a genuinely sparse \
         {which} region correctly"
    );
}

#[tokio::test]
async fn render_sql_matches_the_bytes_it_claims_to_produce() {
    let conn = conn_args();
    let testdb = format!("driver_render_live_test_{}", std::process::id());
    let db = setup_fixture_db(&conn, &testdb).await;

    // --- sparse framebuffer/palette ---
    run_sparse_case(&conn, &testdb, "fb", 100).await;
    run_sparse_case(&conn, &testdb, "pal", 50).await;

    // --- a real refemu-captured frame ---
    truncate_fixture_tables(&db).await;
    let refemu_bin = std::env::var("REFEMU").unwrap_or_else(|_| {
        repo_root()
            .join("target/release/refemu")
            .display()
            .to_string()
    });
    let bin = repo_root().join("rom/build/doom-rv32im.bin");
    let manifest = repo_root().join("rom/build/manifest.json");
    let pinned_hash = repo_root().join("rom/PINNED_HASH");
    let fixture_path = std::env::temp_dir().join(format!(
        "clickdoom-render-live-{}.rsnap",
        std::process::id()
    ));

    let status = Command::new(&refemu_bin)
        .arg("run")
        .arg(&bin)
        .arg("--manifest")
        .arg(&manifest)
        .arg("--pinned-hash")
        .arg(&pinned_hash)
        .arg("--stop-at")
        .arg("frame:0")
        .arg("--max-instructions")
        .arg(TARGET_ICOUNT.to_string())
        .arg("--expect-icount")
        .arg(TARGET_ICOUNT.to_string())
        .arg("--expect-fbhash")
        .arg(EXPECTED_FBHASH)
        .arg("--dump-frame")
        .arg(&fixture_path)
        .status()
        .unwrap_or_else(|e| panic!("running {}: {e}", refemu_bin));
    assert!(status.success(), "{refemu_bin} exited with {status}");

    let snapshot = Snapshot::read(&fixture_path, &["framebuffer", "palette"]).unwrap();
    let _ = std::fs::remove_file(&fixture_path);
    assert_eq!(snapshot.header.kind, Kind::Frame);
    let frame = snapshot
        .header
        .frame
        .clone()
        .expect("a frame capture carries a frame");
    assert_eq!(snapshot.header.fbhash.as_deref(), Some(EXPECTED_FBHASH));
    let fb_bytes = snapshot.section("framebuffer").unwrap().to_vec();
    let pal_bytes = snapshot.section("palette").unwrap().to_vec();
    assert_eq!(fb_bytes.len(), 64_000);
    assert_eq!(pal_bytes.len(), 768);

    let fb_word_rows: Vec<WordRow> = fb_bytes
        .chunks_exact(4)
        .enumerate()
        .map(|(i, w)| WordRow {
            word_addr: i as u32,
            value: u32::from_le_bytes([w[0], w[1], w[2], w[3]]),
            version: frame.retired_icount,
        })
        .collect();
    let pal_word_rows: Vec<WordRow> = pal_bytes
        .chunks_exact(4)
        .enumerate()
        .map(|(i, w)| WordRow {
            word_addr: i as u32,
            value: u32::from_le_bytes([w[0], w[1], w[2], w[3]]),
            version: frame.retired_icount,
        })
        .collect();
    db.insert_all("framebuffer", fb_word_rows.into_iter())
        .await
        .unwrap();
    db.insert_all("palette", pal_word_rows.into_iter())
        .await
        .unwrap();
    insert_batch_commit_row(&db, &testdb, frame.retired_icount, frame.frame_no).await;

    db.run(&render::frame_readout_sql(&testdb)).await.unwrap();
    let rows: u64 = db
        .fetch_one(&format!("SELECT count() FROM {testdb}.frames_out"))
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let actual_fbhash = read_fbhash(&db, &testdb).await;
    assert_eq!(
        actual_fbhash, EXPECTED_FBHASH,
        "frame_readout_sql() reconstructed fb/palette whose fb_hash does not reproduce the SPEC checkpoint oracle"
    );

    // ppm_render_sql() on the same real, fb_hash-verified frame, checked
    // against an independent re-derivation from the frame's own raw bytes
    // -- a second, non-tautological proof on the same known-correct frame.
    let ppm_sql = render::ppm_render_sql(&testdb, FB_WIDTH, FB_HEIGHT);
    let actual_ppm: Bytes = db.fetch_one(&ppm_sql).await.unwrap();
    let mut expected_ppm = format!("P6\n{FB_WIDTH} {FB_HEIGHT}\n255\n").into_bytes();
    for &idx in &fb_bytes {
        let at = idx as usize * 3;
        expected_ppm.extend_from_slice(&pal_bytes[at..at + 3]);
    }
    assert_eq!(
        actual_ppm.as_ref(),
        expected_ppm.as_slice(),
        "ppm_render_sql() did not byte-match an independent re-derivation from the same real frame"
    );

    // --- a small hand-computed synthetic frame ---
    db.run(&format!("TRUNCATE TABLE {testdb}.frames_out"))
        .await
        .unwrap();
    // 2x2: top-left=red(idx0), top-right=green(idx1), bottom-left=blue(idx2), bottom-right=yellow(idx3).
    let mut pal_hex = String::from("ff000000ff000000ffffff00");
    pal_hex.push_str(&"00".repeat(768 - 12));
    db.run(&format!(
        "INSERT INTO {testdb}.frames_out (frame_no, committed_icount, fb, palette) \
         VALUES (0, 1, unhex('00010203'), unhex('{pal_hex}'))"
    ))
    .await
    .unwrap();

    let ansi_sql = render::ansi_render_sql(&testdb, 2, 2);
    let actual_ansi: String = db.fetch_one(&ansi_sql).await.unwrap();
    let esc = '\u{1b}';
    let cell = |fg: (u8, u8, u8), bg: (u8, u8, u8)| {
        format!(
            "{esc}[38;2;{};{};{}m{esc}[48;2;{};{};{}m\u{2580}",
            fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
        )
    };
    let expected_ansi = format!(
        "{}{}{esc}[0m",
        cell((255, 0, 0), (0, 0, 255)),
        cell((0, 255, 0), (255, 255, 0))
    );
    assert_eq!(
        actual_ansi, expected_ansi,
        "ansi_render_sql() did not byte-match the hand-computed escape sequence"
    );

    let ppm_synth_sql = render::ppm_render_sql(&testdb, 2, 2);
    let actual_ppm_synth: Bytes = db.fetch_one(&ppm_synth_sql).await.unwrap();
    let mut expected_ppm_synth = b"P6\n2 2\n255\n".to_vec();
    expected_ppm_synth.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    assert_eq!(
        actual_ppm_synth.as_ref(),
        expected_ppm_synth.as_slice(),
        "ppm_render_sql() did not byte-match the hand-computed synthetic PPM"
    );

    let admin = db_at(&conn, "default");
    admin
        .run(&format!("DROP DATABASE IF EXISTS {testdb}"))
        .await
        .unwrap();
}
