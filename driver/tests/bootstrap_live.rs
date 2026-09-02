//! Live proof for `bootstrap.rs`, run against a real ClickHouse server.
//!
//! Seeding writes exactly one `batch_id = 0` row carrying the reset state,
//! and a second seed against the same database leaves that row alone rather
//! than adding a competing one. The second half is what matters: the driver
//! seeds on every start, including a restart part-way through a long run, so
//! a seed that inserted twice would give the first real batch two candidate
//! previous states to read.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`/`clickdoom`).

#![cfg(feature = "clickhouse-tests")]

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::emulation::bootstrap::{self, RESET_REGS, Seeded};
use clickdoom_driver::sql::split_statements;
use clickdoom_spec::RAM_BASE;
use clickhouse::Row;
use serde::Deserialize;

/// The reset row's columns, read back. A tuple cannot carry `regs`, since
/// `Vec<u32>` is not a scalar column type.
#[derive(Row, Deserialize)]
struct ResetRow {
    pc: u32,
    regs: Vec<u32>,
    icount: u64,
    keyq_pos: u64,
}

const SCHEMA_SQL: &str = include_str!("../../sqlcpu/schema.sql");

fn conn_args() -> ConnArgs {
    ConnArgs {
        host: std::env::var("CLICKHOUSE_HOST").unwrap_or_else(|_| "localhost".to_owned()),
        port: std::env::var("CLICKHOUSE_HTTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8123),
        user: "default".to_owned(),
        database: "default".to_owned(),
        password: std::env::var("CLICKHOUSE_PASSWORD").ok(),
    }
}

fn db_at(conn: &ConnArgs, database: &str) -> Db {
    let mut c = conn.clone();
    c.database = database.to_owned();
    c.connect()
}

/// Materialises the real `sqlcpu/schema.sql` under a private name, the same
/// rename `preflight`'s schema gate uses. A copy of the schema would be free
/// to drift from the one the driver actually writes to.
async fn setup(conn: &ConnArgs, testdb: &str) -> Db {
    let admin = db_at(conn, "default");
    admin
        .run(&format!("DROP DATABASE IF EXISTS {testdb}"))
        .await
        .unwrap();
    let qualified = SCHEMA_SQL
        .replace("clickdoom.", &format!("{testdb}."))
        .replace(
            "CREATE DATABASE IF NOT EXISTS clickdoom;",
            &format!("CREATE DATABASE IF NOT EXISTS {testdb};"),
        );
    for statement in split_statements(&qualified) {
        admin.run(statement).await.unwrap();
    }
    db_at(conn, testdb)
}

#[tokio::test]
async fn seeding_writes_the_reset_state_once_and_a_replay_adds_nothing() {
    let conn = conn_args();
    let testdb = format!("clickdoom_bootstrap_live_test_{}", std::process::id());
    let db = setup(&conn, &testdb).await;

    let first = bootstrap::seed(&db, &RESET_REGS).await.unwrap();
    assert!(
        matches!(first, Seeded::Fresh { .. }),
        "the first seed into an empty database reported {first:?}"
    );

    let row: ResetRow = db
        .fetch_one(&format!(
            "SELECT pc, regs, icount, keyq_pos FROM {testdb}.batch_commit WHERE batch_id = 0"
        ))
        .await
        .unwrap();
    assert_eq!(
        row.pc, RAM_BASE,
        "the reset row's pc is not the reset vector"
    );
    assert_eq!(
        row.regs.len(),
        31,
        "the reset row carries the wrong register count"
    );
    assert!(
        row.regs.iter().all(|&r| r == 0),
        "a reset register is non-zero"
    );
    assert_eq!(row.icount, 0);
    assert_eq!(row.keyq_pos, 0);

    let second = bootstrap::seed(&db, &RESET_REGS).await.unwrap();
    assert!(
        matches!(second, Seeded::AlreadySeeded),
        "seeding a database that already holds batch_id=0 reported {second:?}"
    );
    let rows: u64 = db
        .fetch_one(&format!(
            "SELECT count() FROM {testdb}.batch_commit WHERE batch_id = 0"
        ))
        .await
        .unwrap();
    assert_eq!(
        rows, 1,
        "a replayed seed left more than one batch_id=0 row, so the first real batch has two previous states to choose from"
    );

    db_at(&conn, "default")
        .run(&format!("DROP DATABASE IF EXISTS {testdb}"))
        .await
        .unwrap();
}
