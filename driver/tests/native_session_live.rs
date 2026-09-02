//! Live proof for `native::session`, run against a real ClickHouse server.
//!
//! `native_stream_live.rs` shows one statement stays open. This shows the
//! two of them driven as a session:
//!
//!   * 20 tics and their frames in order, with the renderer reading the
//!     state row the simulation wrote for the same tic, and the per-tic and
//!     per-frame waits reported;
//!   * a simulation statement killed under the session, which has to show
//!     up as a tic that never lands, and `recover` has to reopen it and
//!     resume with the chain unbroken.
//!
//! The statements here are trivial stand-ins. The real ones are generated
//! elsewhere; what this covers is the driving, not the SQL.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use std::time::Duration; // purity-ok: pacing and timeouts in the harness, never a value a statement reads

use clickdoom_driver::client::ConnArgs;
use clickdoom_driver::native::session::{
    Frame, RENDER_INPUT_SCHEMA, SIM_INPUT_SCHEMA, Session, SessionError,
};
use tokio::time::Instant; // purity-ok: measuring what the driver loop waits, never a value a statement reads

/// One DOOM tic at 35 Hz.
const TIC: Duration = Duration::from_micros(28_571);

/// Frame sizes, as `NATIVE.md` fixes them.
const FB_BYTES: usize = 64_000;
const PALETTE_BYTES: usize = 768;
const RGB32_BYTES: usize = 320 * 200 * 4;

/// How long one frame may take to appear before the test gives up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

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

/// A private database holding the two tables a session writes.
async fn setup(database: &str) -> ConnArgs {
    let admin = conn_args("default").connect();
    for sql in [
        format!("DROP DATABASE IF EXISTS {database}"),
        format!("CREATE DATABASE {database}"),
        format!(
            "CREATE TABLE {database}.native_state \
             (tic UInt32, leveltime UInt32, keys UInt32, source UInt8, \
              mouse_dx Int16, mouse_dy Int16) \
             ENGINE = Join(ANY, LEFT, tic)"
        ),
        format!(
            "CREATE TABLE {database}.native_frames \
             (frame UInt32, fb String, palette String, rgb32 String, fb_hash String) \
             ENGINE = Join(ANY, LEFT, frame)"
        ),
    ] {
        admin
            .run(&sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    conn_args(database)
}

async fn teardown(database: &str) {
    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .unwrap_or_else(|e| panic!("dropping {database}: {e}"));
}

/// Accumulates the tic number into `leveltime`, so tic t holds
/// `t * (t + 1) / 2` only if every tic before it landed, in order.
fn sim_statement(database: &str) -> String {
    format!(
        "INSERT INTO {database}.native_state \
         SELECT tic, \
                tic + joinGet('{database}.native_state', 'leveltime', toUInt32(tic - 1)) \
                    AS leveltime, \
                keys, source, mouse_dx, mouse_dy \
         FROM input('{SIM_INPUT_SCHEMA}') WHERE tic > 0"
    )
}

/// Builds a frame out of the state row for the tic it is given, so a frame
/// that reads the wrong tic is visible in its bytes.
fn render_statement(database: &str) -> String {
    format!(
        "INSERT INTO {database}.native_frames \
         SELECT frame, \
                repeat(char(toUInt8(joinGet('{database}.native_state', 'keys', \
                    toUInt32(tic)) % 256)), {FB_BYTES}) AS fb, \
                repeat(char(melt_step), {PALETTE_BYTES}) AS palette, \
                repeat(char(1), {RGB32_BYTES}) AS rgb32, \
                lpad(lower(hex(toUInt64(joinGet('{database}.native_state', 'leveltime', \
                    toUInt32(tic))))), 16, '0') AS fb_hash \
         FROM input('{RENDER_INPUT_SCHEMA}') WHERE frame > 0"
    )
}

/// The `leveltime` the simulation statement above produces for `tic`.
fn leveltime(tic: u32) -> u64 {
    u64::from(tic) * u64::from(tic + 1) / 2
}

fn melt_step(tic: u32) -> u8 {
    (tic % 7) as u8
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let last = sorted.len().saturating_sub(1) as f64;
    sorted[(last * fraction).round() as usize]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1e3
}

/// Runs one tic and the frame that reads it, returning what each waited.
async fn run_tic(session: &Session, tic: u32) -> (Duration, Duration, Frame) {
    session
        .feed_sim(tic, 1, tic, -(tic as i16), tic as i16)
        .unwrap_or_else(|e| panic!("feeding tic {tic}: {e}"));
    let waited = session
        .wait_sim(tic)
        .await
        .unwrap_or_else(|e| panic!("waiting for tic {tic}: {e}"));

    let started = Instant::now(); // purity-ok: measuring the frame wait, see the import
    session
        .feed_render(tic, tic, melt_step(tic))
        .unwrap_or_else(|e| panic!("feeding frame {tic}: {e}"));
    loop {
        let polled = session
            .poll_frame(tic)
            .await
            .unwrap_or_else(|e| panic!("polling frame {tic}: {e}"));
        if let Some(frame) = polled {
            return (waited, started.elapsed(), frame);
        }
        assert!(
            started.elapsed() < FRAME_TIMEOUT,
            "frame {tic} was still not readable after {FRAME_TIMEOUT:?}"
        );
    }
}

fn check_frame(frame: &Frame, tic: u32) {
    assert_eq!(frame.frame, tic);
    assert_eq!(frame.fb.len(), FB_BYTES, "frame {tic}");
    assert_eq!(frame.palette.len(), PALETTE_BYTES, "frame {tic}");
    assert_eq!(frame.rgb32.len(), RGB32_BYTES, "frame {tic}");
    assert!(
        frame.fb.iter().all(|b| u32::from(*b) == tic % 256),
        "frame {tic} was built from another tic's state row"
    );
    assert!(
        frame.palette.iter().all(|b| *b == melt_step(tic)),
        "frame {tic} did not get its melt step"
    );
    assert_eq!(
        frame.fb_hash,
        format!("{:016x}", leveltime(tic)),
        "frame {tic} read the wrong state row"
    );
}

#[tokio::test]
async fn a_session_runs_tics_and_their_frames_in_order() {
    let database = format!("native_session_{}", std::process::id());
    let conn = setup(&database).await;
    let session = Session::open(
        &conn,
        &database,
        &sim_statement(&database),
        &render_statement(&database),
    )
    .await
    .expect("opening the session");

    assert_eq!(
        session.resume_point().await.expect("an empty state table"),
        1,
        "an empty session starts at tic 1"
    );

    let tics = 20u32;
    let mut sim_waits = Vec::with_capacity(tics as usize);
    let mut frame_waits = Vec::with_capacity(tics as usize);
    for tic in 1..=tics {
        let (sim, frame_wait, frame) = run_tic(&session, tic).await;
        check_frame(&frame, tic);
        sim_waits.push(sim);
        frame_waits.push(frame_wait);
        let spent = sim + frame_wait;
        if let Some(rest) = TIC.checked_sub(spent) {
            tokio::time::sleep(rest).await;
        }
    }

    sim_waits.sort_unstable();
    frame_waits.sort_unstable();
    println!(
        "over {tics} tics: wait_sim p50 {:.2} ms p99 {:.2} ms, \
         frame ({} bytes) p50 {:.2} ms p99 {:.2} ms",
        millis(percentile(&sim_waits, 0.50)),
        millis(percentile(&sim_waits, 0.99)),
        FB_BYTES + PALETTE_BYTES + RGB32_BYTES,
        millis(percentile(&frame_waits, 0.50)),
        millis(percentile(&frame_waits, 0.99)),
    );

    assert_eq!(
        session
            .resume_point()
            .await
            .expect("reading the resume point"),
        tics + 1
    );
    assert!(
        session
            .poll_frame(tics + 1)
            .await
            .expect("polling a frame that was never fed")
            .is_none(),
        "a frame nothing wrote has to read as absent"
    );

    let db = conn.connect_uncompressed();
    let (rows, chained, keys, mouse, source) = db
        .fetch_one::<(u64, u64, u64, u64, u64)>(&format!(
            "SELECT count(), \
                    countIf(leveltime = tic * (tic + 1) / 2), \
                    countIf(keys = tic), \
                    countIf(mouse_dx = -toInt16(tic) AND mouse_dy = toInt16(tic)), \
                    countIf(source = 1) \
             FROM {database}.native_state"
        ))
        .await
        .expect("reading the state table back");
    let want = u64::from(tics);
    assert_eq!(rows, want, "the padding row must not be stored");
    assert_eq!(chained, want, "a tic did not see the tic before it");
    assert_eq!(keys, want, "a key word did not reach the statement");
    assert_eq!(mouse, want, "a mouse delta did not reach the statement");
    assert_eq!(source, want, "a source flag did not reach the statement");

    session
        .close()
        .await
        .expect("both statements ran to the end");
    teardown(&database).await;
}

#[tokio::test]
async fn a_killed_statement_is_found_and_recovered() {
    let database = format!("native_session_kill_{}", std::process::id());
    let conn = setup(&database).await;
    let admin = conn_args("default").connect();
    let mut session = Session::open(
        &conn,
        &database,
        &sim_statement(&database),
        &render_statement(&database),
    )
    .await
    .expect("opening the session");

    let before_kill = 5u32;
    for tic in 1..=before_kill {
        let (_, _, frame) = run_tic(&session, tic).await;
        check_frame(&frame, tic);
    }

    // Kill the simulation, and prove the kill had something to kill. A
    // KILL that matched nothing would leave this test passing on a session
    // that never broke.
    let sim_query_id = session.sim_query_id().to_owned();
    let running = admin
        .fetch_one::<u64>(&format!(
            "SELECT count() FROM system.processes WHERE query_id = '{sim_query_id}'"
        ))
        .await
        .expect("reading system.processes");
    assert_eq!(running, 1, "the simulation statement is not running");
    // ASYNC, because SYNC waits for the statement to actually stop and a
    // statement blocked on its own request body only stops when the next
    // block reaches it, or when the server's `http_receive_timeout` fires.
    let killed = admin
        .fetch_all::<(String, String, String, String)>(&format!(
            "KILL QUERY WHERE query_id = '{sim_query_id}' ASYNC"
        ))
        .await
        .expect("issuing the kill");
    assert_eq!(killed.len(), 1, "the kill matched nothing: {killed:?}");

    // Feeding the next tic is what delivers the block the cancelled
    // statement notices. Whether the session finds out from the row it
    // sends or from the tic that never lands depends on which reaches it
    // first; both mean the same thing.
    let next = before_kill + 1;
    let error = match session.feed_sim(next, 1, next, 0, 0) {
        Err(error) => error,
        Ok(()) => session
            .wait_sim(next)
            .await
            .expect_err("the killed statement cannot write the tic"),
    };
    match &error {
        SessionError::Sim { .. } => {}
        SessionError::TicTimeout { tic, .. } => assert_eq!(*tic, next),
        other => panic!("the session did not notice the kill: {other}"),
    }
    println!("the session noticed the kill as: {error}");

    let recovery = tokio::time::timeout(Duration::from_secs(30), session.recover(&conn))
        .await
        .expect("recover has to finish")
        .expect("recover has to reopen both statements");
    assert_eq!(
        recovery.resume_tic,
        before_kill + 1,
        "the session resumes at the first tic that was not committed"
    );
    let sim_error = recovery
        .sim
        .as_ref()
        .expect("the killed statement has to report why it ended");
    assert!(
        sim_error.to_string().contains("QUERY_WAS_CANCELLED")
            || sim_error.to_string().contains("Query was cancelled"),
        "the kill has to reach the caller: {sim_error}"
    );
    assert_ne!(
        session.sim_query_id(),
        sim_query_id,
        "a reopened statement runs under a new query id"
    );

    for tic in recovery.resume_tic..recovery.resume_tic + 5 {
        let (_, _, frame) = run_tic(&session, tic).await;
        check_frame(&frame, tic);
    }

    let db = conn.connect_uncompressed();
    let (rows, chained) = db
        .fetch_one::<(u64, u64)>(&format!(
            "SELECT count(), countIf(leveltime = tic * (tic + 1) / 2) \
             FROM {database}.native_state"
        ))
        .await
        .expect("reading the state table back");
    assert_eq!(rows, u64::from(before_kill + 5));
    assert_eq!(chained, rows, "the chain did not survive the recovery");

    session
        .close()
        .await
        .expect("the reopened statements run to the end");
    teardown(&database).await;
}
