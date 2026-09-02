//! Live proof for `clickdoom native diff`, run as a caller runs it.
//!
//!   * a run whose probe records none of the tics it ran says so and fails,
//!     rather than reporting agreement over nothing;
//!   * a run over tics the probe does record reports the first field that
//!     differs, with the tic and both values, and exits 3;
//!   * the probe rows land in `probe_state` and not in `native_state`,
//!     because the two are the sides of the comparison.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`) and the committed
//! probe fixture.

#![cfg(feature = "clickhouse-tests")]

use std::process::Command;

mod support;

use support::{committed_fixture, conn_args, repo_root};

/// The gametics the committed fixture records: the melt's, the first
/// gameplay tic, the one after it, and one from the middle of the demo.
const FIRST_RECORDED_TIC: u32 = 2;

fn clickdoom(database: &str, args: &[&str]) -> (i32, String) {
    let conn = conn_args(database);
    let output = Command::new(env!("CARGO_BIN_EXE_clickdoom"))
        .current_dir(repo_root())
        .args(args)
        .args([
            "--host",
            &conn.host,
            "--port",
            &conn.port.to_string(),
            "--database",
            database,
            "--password",
            &conn.resolved_password(),
        ])
        .output()
        .unwrap_or_else(|e| panic!("running clickdoom {args:?}: {e}"));
    let mut printed = String::from_utf8_lossy(&output.stdout).into_owned();
    printed.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), printed)
}

#[tokio::test]
async fn a_differential_run_reports_the_first_field_that_differs() {
    let database = format!("clickdoom_native_diff_{}", std::process::id());
    let (code, printed) = clickdoom(&database, &["native", "load", "--fresh"]);
    assert_eq!(code, 0, "{printed}");

    let fixture = committed_fixture();
    let probe = fixture.to_str().expect("a path");

    // The fixture's earliest gametic is 2, so a run of one tic compares
    // nothing. Reporting agreement there would be a check that never ran.
    let short = (FIRST_RECORDED_TIC - 1).to_string();
    let (code, printed) = clickdoom(&database, &["native", "diff", &short, "--probe", probe]);
    assert_eq!(code, 1, "{printed}");
    assert!(printed.contains("nothing was compared"), "{printed}");

    let tics = FIRST_RECORDED_TIC.to_string();
    let (code, printed) = clickdoom(&database, &["native", "diff", &tics, "--probe", probe]);
    assert_eq!(code, 3, "the simulation is not complete: {printed}");
    assert!(
        printed.contains(&format!("tic {FIRST_RECORDED_TIC} ")),
        "{printed}"
    );
    assert!(printed.contains("against the probe's"), "{printed}");

    // The two sides are two tables. A diff that copied the probe into
    // native_state would compare the run against itself and always agree.
    let db = conn_args(&database).connect();
    let staged: u64 = db
        .fetch_one(&format!("SELECT count() FROM {database}.probe_state"))
        .await
        .expect("the staging table");
    assert!(staged > 0, "the probe rows did not land");
    let ours: u64 = db
        .fetch_one(&format!("SELECT count() FROM {database}.native_state"))
        .await
        .expect("the state table");
    assert_eq!(
        ours,
        u64::from(FIRST_RECORDED_TIC) + 1,
        "native_state holds the run's own tics and the level's first row, and nothing else"
    );

    let (code, printed) = clickdoom(
        &database,
        &["native", "diff", &tics, "--probe", probe, "--summary"],
    );
    assert_eq!(code, 3, "{printed}");
    assert!(printed.contains("first_tic="), "{printed}");

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .expect("the database is dropped");
}
