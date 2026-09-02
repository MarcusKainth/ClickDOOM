//! Live proof for `native::stream`, run against a real ClickHouse server.
//!
//! The unit tests fix the bytes and the settings. Nothing there shows that a
//! statement stays open, that a row lands while it is open, or that a broken
//! statement is ever reported, so this runs those for real:
//!
//!   * 100 rows at 35 Hz through one statement, each read back before the
//!     next is sent, with the chain the statement builds through `joinGet`
//!     checked row by row and the send-to-visible latency reported;
//!   * a statement with a syntax error, whose message has to reach the
//!     caller when the body closes;
//!   * a statement that fails on one row, which keeps taking rows and
//!     commits none of them from there on;
//!   * a 200 KB statement, which no URL parameter could carry, so it proves
//!     the statement really does lead the request body.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use std::time::{Duration, Instant}; // purity-ok: pacing and latency measurement in the harness, never a value a statement reads

use bytes::Bytes;
use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::native::rowbinary::Row;
use clickdoom_driver::native::settings::resident_settings;
use clickdoom_driver::native::{Resident, ResidentError};

/// The schema every statement here reads. `pad` exists for the padding row
/// the transport writes behind the statement.
const INPUT_SCHEMA: &str = "tic UInt32, pad String";

/// One DOOM tic at 35 Hz.
const TIC: Duration = Duration::from_micros(28_571);

/// How many tics the pacing test streams.
const TICS: u32 = 100;

/// What the median send-to-visible latency has to beat. A tic's budget is
/// 28.6 ms and the transport is allowed a small part of it.
const P50_LIMIT: Duration = Duration::from_millis(5);

/// How long one row may take to appear before the test gives up.
const VISIBLE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a broken statement may take to report itself.
const FAILURE_TIMEOUT: Duration = Duration::from_secs(20);

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

/// A private database with `table` in it as a `Join` engine keyed by tic,
/// dropped and recreated so a rerun starts clean.
async fn setup(database: &str, columns: &str) -> ConnArgs {
    let admin = conn_args("default").connect();
    admin
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .unwrap_or_else(|e| panic!("dropping {database}: {e}"));
    admin
        .run(&format!("CREATE DATABASE {database}"))
        .await
        .unwrap_or_else(|e| panic!("creating {database}: {e}"));
    admin
        .run(&format!(
            "CREATE TABLE {database}.pairs ({columns}) ENGINE = Join(ANY, LEFT, tic)"
        ))
        .await
        .unwrap_or_else(|e| panic!("creating {database}.pairs: {e}"));
    conn_args(database)
}

async fn teardown(database: &str) {
    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .unwrap_or_else(|e| panic!("dropping {database}: {e}"));
}

/// One input row: the tic, and an empty `pad`.
fn row(tic: u32) -> Bytes {
    let mut row = Row::with_capacity(8);
    row.u32(tic).bytes(b"");
    row.finish()
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let last = sorted.len().saturating_sub(1) as f64;
    sorted[(last * fraction).round() as usize]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

/// Sends the row for `tic` and waits for the statement to write it, polling
/// the destination. Returns how long that took.
async fn send_and_wait(resident: &Resident, db: &Db, database: &str, tic: u32) -> Duration {
    let sent = Instant::now(); // purity-ok: measuring send-to-visible, see the import
    resident
        .send(row(tic))
        .unwrap_or_else(|e| panic!("tic {tic}: {e}"));
    let query = format!("SELECT joinGet('{database}.pairs', 'doubled', toUInt32({tic}))");
    loop {
        let doubled = db
            .fetch_one::<u32>(&query)
            .await
            .unwrap_or_else(|e| panic!("polling tic {tic}: {e}"));
        if doubled == tic * 2 {
            return sent.elapsed();
        }
        assert!(
            resident.alive(),
            "tic {tic}: the statement ended before the row appeared"
        );
        assert!(
            sent.elapsed() < VISIBLE_TIMEOUT,
            "tic {tic} was still not readable after {VISIBLE_TIMEOUT:?}"
        );
    }
}

#[tokio::test]
async fn one_statement_takes_a_hundred_tics_and_chains_them() {
    let database = format!("native_stream_{}", std::process::id());
    let conn = setup(&database, "tic UInt32, doubled UInt32, previous UInt32").await;
    let db = conn.connect_uncompressed();

    // Every row reads the row before it out of the destination table, so a
    // row that lands early or late shows up as a wrong `previous`.
    let statement = format!(
        "INSERT INTO {database}.pairs \
         SELECT tic, tic * 2 AS doubled, \
         joinGet('{database}.pairs', 'doubled', toUInt32(tic - 1)) AS previous \
         FROM input('{INPUT_SCHEMA}') WHERE tic > 0"
    );
    let resident = Resident::open(
        &conn,
        &statement,
        INPUT_SCHEMA,
        &resident_settings(statement.len()),
    )
    .await
    .expect("opening the resident statement");

    let mut visible = Vec::with_capacity(TICS as usize);
    for tic in 1..=TICS {
        let took = send_and_wait(&resident, &db, &database, tic).await;
        visible.push(took);
        if let Some(rest) = TIC.checked_sub(took) {
            tokio::time::sleep(rest).await;
        }
    }
    resident
        .close()
        .await
        .expect("the statement ran to the end");

    visible.sort_unstable();
    let p50 = percentile(&visible, 0.50);
    let p99 = percentile(&visible, 0.99);
    println!(
        "send-to-visible over {TICS} tics: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        millis(p50),
        millis(p99),
        millis(visible[visible.len() - 1])
    );

    let (rows, doubled, chained) = db
        .fetch_one::<(u64, u64, u64)>(&format!(
            "SELECT count(), \
                    countIf(doubled = tic * 2), \
                    countIf(previous = if(tic = 1, 0, (tic - 1) * 2)) \
             FROM {database}.pairs"
        ))
        .await
        .expect("reading the destination back");
    assert_eq!(rows, u64::from(TICS), "the padding row must not be stored");
    assert_eq!(doubled, u64::from(TICS), "a row carries the wrong value");
    assert_eq!(
        chained,
        u64::from(TICS),
        "a row did not see the row before it"
    );

    assert!(
        p50 < P50_LIMIT,
        "send-to-visible p50 was {:.2} ms, over the {:.2} ms this asserts",
        millis(p50),
        millis(P50_LIMIT)
    );
    teardown(&database).await;
}

#[tokio::test]
async fn a_statement_the_server_cannot_parse_reports_its_message_on_close() {
    let database = format!("native_stream_broken_{}", std::process::id());
    let conn = setup(&database, "tic UInt32, doubled UInt32").await;

    // The comparison is left dangling, so the server rejects the statement
    // while parsing it.
    let statement = format!(
        "INSERT INTO {database}.pairs SELECT tic, tic * 2 FROM input('{INPUT_SCHEMA}') WHERE tic >"
    );
    let resident = Resident::open(
        &conn,
        &statement,
        INPUT_SCHEMA,
        &resident_settings(statement.len()),
    )
    .await
    .expect("opening a request the server has not parsed yet");

    // The server takes the rows without answering, so nothing here can tell
    // the statement is broken until the body closes.
    for tic in 1..=5u32 {
        resident
            .send(row(tic))
            .unwrap_or_else(|e| panic!("tic {tic}: {e}"));
        tokio::time::sleep(TIC).await;
    }
    assert!(
        resident.alive(),
        "no response is due while the body is open"
    );

    let closed = tokio::time::timeout(FAILURE_TIMEOUT, resident.close())
        .await
        .unwrap_or_else(|_| panic!("close did not report anything within {FAILURE_TIMEOUT:?}"))
        .expect_err("a statement the server rejected is not a clean close");
    let ResidentError::Ended { status, message } = &closed else {
        panic!("expected Ended, got {closed}");
    };
    assert_eq!(*status, Some(400), "{message}");
    assert!(
        message.contains("DB::Exception") && message.contains("Syntax error"),
        "the server's own message has to reach the caller: {message}"
    );
    teardown(&database).await;
}

#[tokio::test]
async fn a_statement_that_fails_on_a_row_stops_writing_and_says_why() {
    let database = format!("native_stream_throws_{}", std::process::id());
    let conn = setup(&database, "tic UInt32, doubled UInt32").await;
    let db = conn.connect_uncompressed();

    // Parses, then fails on the fourth row it is given. This is how a
    // statement dies in the middle of a session.
    let failing_tic = 4u32;
    let statement = format!(
        "INSERT INTO {database}.pairs \
         SELECT tic, toUInt32(tic * 2 + throwIf(tic = {failing_tic}, 'the fourth row')) AS doubled \
         FROM input('{INPUT_SCHEMA}') WHERE tic > 0"
    );
    let resident = Resident::open(
        &conn,
        &statement,
        INPUT_SCHEMA,
        &resident_settings(statement.len()),
    )
    .await
    .expect("opening the resident statement");

    for tic in 1..failing_tic {
        send_and_wait(&resident, &db, &database, tic).await;
        tokio::time::sleep(TIC).await;
    }

    // The row that fails, then more behind it. The server keeps taking them
    // and stays silent, so what shows the statement is dead is that its
    // rows stop landing.
    for tic in failing_tic..failing_tic + 3 {
        resident
            .send(row(tic))
            .unwrap_or_else(|e| panic!("tic {tic}: {e}"));
        tokio::time::sleep(TIC).await;
    }
    let query = format!("SELECT joinGet('{database}.pairs', 'doubled', toUInt32({failing_tic}))");
    let landed = db
        .fetch_one::<u32>(&query)
        .await
        .expect("polling the row that failed");
    assert_eq!(landed, 0, "the row that failed must not be stored");

    let closed = tokio::time::timeout(FAILURE_TIMEOUT, resident.close())
        .await
        .unwrap_or_else(|_| panic!("close did not report anything within {FAILURE_TIMEOUT:?}"))
        .expect_err("a statement that threw is not a clean close");
    let ResidentError::Ended { status, message } = &closed else {
        panic!("expected Ended, got {closed}");
    };
    assert!(status.is_some(), "the server answered: {message}");
    assert!(
        message.contains("the fourth row"),
        "the server's own message has to reach the caller: {message}"
    );

    let stored = db
        .fetch_one::<u64>(&format!("SELECT count() FROM {database}.pairs"))
        .await
        .expect("reading the destination back");
    assert_eq!(
        stored,
        u64::from(failing_tic - 1),
        "only the rows before the failure are committed"
    );
    teardown(&database).await;
}

#[tokio::test]
async fn a_statement_too_large_for_a_url_parameter_opens() {
    let database = format!("native_stream_large_{}", std::process::id());
    let conn = setup(&database, "tic UInt32, doubled UInt32").await;
    let db = conn.connect_uncompressed();

    // A URL parameter stops at about 64 KB. This is over 200 KB, so it can
    // only reach the server as the head of the request body.
    let entries = 28_600u32;
    let mut lookup = String::with_capacity(entries as usize * 7);
    for value in 0..entries {
        if value > 0 {
            lookup.push(',');
        }
        lookup.push_str(&(100_000 + value).to_string());
    }
    let statement = format!(
        "INSERT INTO {database}.pairs \
         WITH [{lookup}] AS lut \
         SELECT tic, lut[toUInt32(tic)] AS doubled \
         FROM input('{INPUT_SCHEMA}') WHERE tic > 0"
    );
    assert!(
        statement.len() > 200_000,
        "this test only means something above 64 KB, and it is {} bytes",
        statement.len()
    );

    let resident = Resident::open(
        &conn,
        &statement,
        INPUT_SCHEMA,
        &resident_settings(statement.len()),
    )
    .await
    .expect("opening a 200 KB statement");

    for tic in 1..=3u32 {
        let started = Instant::now(); // purity-ok: a harness timeout, see the import
        resident
            .send(row(tic))
            .unwrap_or_else(|e| panic!("tic {tic}: {e}"));
        let query = format!("SELECT joinGet('{database}.pairs', 'doubled', toUInt32({tic}))");
        loop {
            let got = db
                .fetch_one::<u32>(&query)
                .await
                .unwrap_or_else(|e| panic!("polling tic {tic}: {e}"));
            if got == 100_000 + tic - 1 {
                break;
            }
            assert!(
                resident.alive(),
                "tic {tic}: the statement ended before the row appeared"
            );
            assert!(
                started.elapsed() < FAILURE_TIMEOUT,
                "tic {tic} was still not readable after {FAILURE_TIMEOUT:?}"
            );
        }
    }
    resident
        .close()
        .await
        .expect("the statement ran to the end");
    teardown(&database).await;
}
