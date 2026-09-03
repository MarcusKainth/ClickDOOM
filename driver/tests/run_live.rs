//! The batch-loop runner's checkpoint comparison, against a real server and
//! the pinned ROM.
//!
//! The point under test is the register cadence. A batch is far longer than
//! `CHECKPOINT_INTERVAL`, so the boundaries it crosses are only observable
//! through the checkpoints the fold records, and until they are compared a
//! run reads 255 of every 256 trace lines and looks at none of them.
//!
//! Two phases against one loaded database, in order. The first runs against
//! the committed trace and has to reach its target with the comparison count
//! the cadence predicts. The second corrupts one trace line at a boundary
//! that is not a `RAM_HASH_INTERVAL` one, which is exactly the line the
//! coarse comparison could never reach, and the run has to stop there.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`) and the pinned
//! ROM built. Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes it.

#![cfg(feature = "clickhouse-tests")]

use std::path::{Path, PathBuf};

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::emulation::run::{self, RunError, Stop};
use clickdoom_driver::emulation::{decode, rom};
use clickdoom_driver::sql::split_statements;
use clickdoom_spec::{CHECKPOINT_INTERVAL, Manifest, RAM_BASE, hashed_filename};

const SCHEMA_SQL: &str = include_str!("../../sqlcpu/schema.sql");
const PINNED_HASH: &str = include_str!("../../rom/PINNED_HASH");

/// The instruction counts the two phases stop at. Both are multiples of
/// `CHECKPOINT_INTERVAL` and neither is a `RAM_HASH_INTERVAL` boundary, so
/// every comparison here is a register-only one.
const PHASE_A_TARGET: u64 = 2 * CHECKPOINT_INTERVAL;
const PHASE_B_TARGET: u64 = 4 * CHECKPOINT_INTERVAL;
/// The line phase B corrupts. Strictly inside phase B's own batch, so it is
/// reachable only through the checkpoints the fold recorded.
const CORRUPT_AT: u64 = 3 * CHECKPOINT_INTERVAL;

const K: u32 = 60_000;
const HWM: u32 = 20_000;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
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

/// The committed trace for the pinned ROM, named after it so a re-pinned
/// ROM cannot silently reuse the previous one's.
fn reference_trace() -> PathBuf {
    let name = hashed_filename("demo-boot-to-first-frame", PINNED_HASH.trim(), ".tsv")
        .expect("rom/PINNED_HASH is a full sha256");
    repo_root().join("refemu/reference_traces").join(name)
}

/// `trace` with the line at `icount` rewritten to carry a wrong reghash,
/// written beside the test's other scratch files.
fn trace_with_corrupt_line(trace: &Path, icount: u64, out: &Path) {
    let text = std::fs::read_to_string(trace).expect("the committed trace is readable");
    let mut corrupted = 0;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let mut fields: Vec<&str> = line.split('\t').collect();
            if fields.first() != Some(&icount.to_string().as_str()) {
                return line.to_owned();
            }
            corrupted += 1;
            fields[2] = "0000000000000000";
            fields.join("\t")
        })
        .collect();
    assert_eq!(
        corrupted, 1,
        "no trace line at icount={icount} to corrupt, so the case would pass for the wrong reason"
    );
    std::fs::write(out, lines.join("\n") + "\n").expect("the scratch trace is writable");
}

async fn provision(database: &str, bin: &Path, manifest_path: &Path) -> Db {
    let admin = conn_args("default").connect();
    admin
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
    admin
        .run(&format!("CREATE DATABASE {database}"))
        .await
        .expect("the database is created");
    let qualified = SCHEMA_SQL
        .replace("clickdoom.", &format!("{database}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {database};"),
        );
    for statement in split_statements(&qualified) {
        admin.run(statement).await.expect("the schema applies");
    }

    let db = conn_args(database).connect();
    rom::load(&db, bin, manifest_path, rom::RAM_WORDS_DEFAULT)
        .await
        .expect("the ROM loads");
    let manifest = Manifest::read(manifest_path).expect("the manifest parses");
    let text_start = manifest.text_start.unwrap_or(RAM_BASE);
    let text_end = manifest.text_end.unwrap_or(RAM_BASE);
    decode::decode(&db, database, text_start / 4, text_end / 4)
        .await
        .expect("the text region decodes");
    db
}

#[tokio::test]
async fn a_run_compares_every_register_checkpoint_and_stops_on_the_first_that_differs() {
    let bin = repo_root().join("rom/build/doom-rv32im.bin");
    let manifest_path = repo_root().join("rom/build/manifest.json");
    assert!(
        bin.exists(),
        "{} is not built. Run `make build-rom`.",
        bin.display()
    );
    let trace = reference_trace();
    let database = format!("clickdoom_driver_run_{}", std::process::id());
    provision(&database, &bin, &manifest_path).await;
    let conn = conn_args(&database);

    // Phase A: the committed trace, to a target two boundaries in.
    let outcome = run::run(
        &conn,
        &run::Args {
            bin: &bin,
            manifest_path: &manifest_path,
            k: K,
            hwm: HWM,
            trace_path: &trace,
            target_icount: PHASE_A_TARGET,
            stop_at_frame: None,
            frame_dir: None,
        },
    )
    .await
    .expect("the run reaches its target against the committed trace");
    assert!(matches!(outcome.stop, Stop::ReachedTarget));
    assert_eq!(outcome.final_icount, PHASE_A_TARGET);
    assert_eq!(
        outcome.reg_checkpoints_compared,
        PHASE_A_TARGET / CHECKPOINT_INTERVAL,
        "one comparison per boundary crossed, not one per batch"
    );

    // Phase B: the same run continued, against a trace whose line at
    // CORRUPT_AT carries a wrong reghash. That icount is not a
    // RAM_HASH_INTERVAL boundary and is not where a batch ends, so only the
    // checkpoints recorded inside the fold reach it.
    let corrupt_trace = std::env::temp_dir().join(format!(
        "clickdoom-run-live-corrupt-{}.tsv",
        std::process::id()
    ));
    trace_with_corrupt_line(&trace, CORRUPT_AT, &corrupt_trace);
    let result = run::run(
        &conn,
        &run::Args {
            bin: &bin,
            manifest_path: &manifest_path,
            k: K,
            hwm: HWM,
            trace_path: &corrupt_trace,
            target_icount: PHASE_B_TARGET,
            stop_at_frame: None,
            frame_dir: None,
        },
    )
    .await;
    let _ = std::fs::remove_file(&corrupt_trace);
    match result {
        Err(RunError::CheckpointMismatch { icount, .. }) => assert_eq!(
            icount, CORRUPT_AT,
            "the run stopped at a different boundary than the corrupted one"
        ),
        Err(other) => panic!("expected a checkpoint mismatch at icount={CORRUPT_AT}, got {other}"),
        Ok(outcome) => panic!(
            "the run reached icount={} without noticing the corrupted line at icount={CORRUPT_AT}",
            outcome.final_icount
        ),
    }

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}
