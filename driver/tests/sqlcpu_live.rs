//! The SQL CPU's own suite, run against a live ClickHouse server.
//!
//! The checks, sequenced against one private database: `sqlcpu/schema.sql`'s
//! own DDL, `sqlcpu/decode.sql` against the committed decode vectors,
//! one-instruction batches against an independent RV32I reference, the
//! riscv-tests corpus run to completion inside the database, and the
//! checkpoint expressions against `clickdoom-spec`'s own hashes.
//!
//! Every instruction executes through
//! [`clickdoom_executor::fold::select_only`], the SQL text the DOOM run
//! itself folds, so what the suite proves correct is the CPU the game runs
//! on.
//!
//! Needs a reachable ClickHouse: `CLICKHOUSE_HOST` and
//! `CLICKHOUSE_HTTP_PORT` place it, `CLICKHOUSE_PASSWORD` authenticates,
//! defaulting to `localhost:8123`. The database is named after the process
//! and dropped when the suite finishes. Behind the `clickhouse-tests`
//! feature, so a run without a server does not silently report a pass.

#![cfg(feature = "clickhouse-tests")]

mod sqlcpu;

use sqlcpu::harness::{self, Report};
use sqlcpu::{checkpoint_format, decode_vectors, execute_vectors, riscv_tests, schema};

/// Every check the run must account for.
///
/// The sequencer reports one outcome per name and the run fails unless all
/// of them arrive. A check the sequencer no longer calls produces no
/// outcome, which a summary line cannot tell from a passing one, so the
/// roster is what makes a missing check a failure instead of a shorter
/// green run.
const REQUIRED_CHECKS: &[&str] = &[
    "schema",
    "decode-vectors",
    "execute-vectors",
    "riscv-tests",
    "checkpoint-format",
];

#[tokio::test]
async fn the_sql_cpu_passes_its_own_suite() {
    let conn = harness::conn_args();
    let database = format!("sqlcpu_suite_{}", std::process::id());
    let admin = harness::db_at(&conn, "default");
    harness::create_database(&admin, &database)
        .await
        .unwrap_or_else(|e| panic!("provisioning {database}: {e}"));
    let db = harness::db_at(&conn, &database);

    let mut report = Report::new(REQUIRED_CHECKS);
    report.record("schema", schema::check(&db, &database).await);
    report.record(
        "decode-vectors",
        decode_vectors::check(&db, &database).await,
    );
    report.record(
        "execute-vectors",
        execute_vectors::check(&db, &database).await,
    );
    report.record("riscv-tests", riscv_tests::check(&db, &database).await);
    report.record("checkpoint-format", checkpoint_format::check(&db).await);
    let outcome = report.finish();

    admin
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .unwrap_or_else(|e| panic!("dropping {database}: {e}"));

    if let Err(failures) = outcome {
        panic!("\n{failures}");
    }
}
