//! Live proof that a session leaves the server the connections it found.
//!
//! Two properties, both about the server's own HTTP connections. A poll has
//! to go out on the connection the poll before it used: a read that leaves
//! its response unfinished makes the client drop the connection, and a
//! session polling every 250 microseconds then opens hundreds of server
//! connections a second. And a session that ends, whether its statements
//! ran to the end or were killed under it, has to bring `HTTPConnection`
//! back to what it was before the session opened.
//!
//! One test, because both counts are server-wide and a second test running
//! beside this one would be reading its connections.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use std::time::Duration; // purity-ok: bounds on what this test waits for, never a value a statement reads

use clickdoom_driver::client::{ConnArgs, Db};
use clickdoom_driver::native::session::{RENDER_INPUT_SCHEMA, SIM_INPUT_SCHEMA, Session};
use tokio::time::Instant; // purity-ok: measuring what this test waits, never a value a statement reads

/// How long one tic or one frame may take before the test gives up.
const ROW_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the server is given to let go of the connections a closed
/// session was using.
const SETTLE: Duration = Duration::from_secs(10);

/// How many times the poll is asked for a frame that is not there. Enough
/// that a connection opened per poll is unmistakable against the handful a
/// session and this test hold between them.
const POLLS: u64 = 300;

/// The most connections `POLLS` polls may open. A poll that reuses its
/// connection opens none; the margin covers the session's own statements
/// and this test's reads landing in the same window.
const OPENED_BUDGET: u64 = 10;

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
             (frame UInt32, fb String, palette String, rgb32 String, fb_hash UInt64) \
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

fn sim_statement(database: &str) -> String {
    format!(
        "INSERT INTO {database}.native_state \
         SELECT tic, tic * 2 AS leveltime, keys, source, mouse_dx, mouse_dy \
         FROM input('{SIM_INPUT_SCHEMA}') WHERE tic > 0"
    )
}

fn render_statement(database: &str) -> String {
    format!(
        "INSERT INTO {database}.native_frames \
         SELECT frame, repeat('x', 8) AS fb, repeat('y', 4) AS palette, \
                repeat('z', 4) AS rgb32, toUInt64(tic + melt_step) AS fb_hash \
         FROM input('{RENDER_INPUT_SCHEMA}') WHERE frame > 0"
    )
}

/// The server's own count of busy HTTP connections, this read included.
async fn busy(admin: &Db) -> i64 {
    admin
        .fetch_one::<i64>("SELECT value FROM system.metrics WHERE metric = 'HTTPConnection'")
        .await
        .expect("reading system.metrics")
}

/// How many HTTP connections the server has accepted since it started.
async fn opened(admin: &Db) -> u64 {
    admin
        .fetch_one::<u64>(
            "SELECT sumIf(value, event = 'HTTPServerConnectionsCreated') FROM system.events",
        )
        .await
        .expect("reading system.events")
}

/// [`busy`] once it has come back down to `baseline`, or the last count
/// read when it does not inside [`SETTLE`].
async fn settled(admin: &Db, baseline: i64) -> i64 {
    let started = Instant::now(); // purity-ok: bounding this wait, see the import
    loop {
        let now = busy(admin).await;
        if now <= baseline || started.elapsed() >= SETTLE {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One tic and the frame that reads it.
async fn run_tic(session: &Session, tic: u32) {
    session
        .feed_sim(tic, 1, tic, 0, 0)
        .unwrap_or_else(|e| panic!("feeding tic {tic}: {e}"));
    session
        .wait_sim(tic, ROW_TIMEOUT)
        .await
        .unwrap_or_else(|e| panic!("waiting for tic {tic}: {e}"));
    session
        .feed_render(tic, tic, 0)
        .unwrap_or_else(|e| panic!("feeding frame {tic}: {e}"));
    session
        .wait_frame(tic, ROW_TIMEOUT)
        .await
        .unwrap_or_else(|e| panic!("waiting for frame {tic}: {e}"));
}

#[tokio::test]
async fn a_session_leaves_the_server_the_connections_it_found() {
    let database = format!("native_connections_{}", std::process::id());
    let conn = setup(&database).await;
    let admin = conn_args("default").connect_uncompressed();
    let baseline = busy(&admin).await;

    let mut session = Session::open(
        &conn,
        &database,
        Some(&sim_statement(&database)),
        Some(&render_statement(&database)),
    )
    .await
    .expect("opening the session");
    for tic in 1..=5 {
        run_tic(&session, tic).await;
    }

    // A poll that leaves its response unfinished costs a connection. The
    // frame asked for is one nothing has written, which is the read a
    // paced run spends most of its polls on.
    let before = opened(&admin).await;
    for _ in 0..POLLS {
        assert!(
            session
                .poll_frame(9_999)
                .await
                .expect("polling a frame that was never fed")
                .is_none()
        );
    }
    let cost = opened(&admin).await - before;
    assert!(
        cost <= OPENED_BUDGET,
        "{POLLS} polls opened {cost} server connections. A poll has to read \
         its response to the end, so its connection goes back in the client's \
         pool instead of being dropped"
    );

    // A statement killed under the session, recovered, and the session
    // driven on: the connections of both the killed statement and the one
    // that replaced it have to go.
    let sim_query_id = session.sim_query_id().to_owned();
    let killed = admin
        .fetch_all::<(String, String, String, String)>(&format!(
            "KILL QUERY WHERE query_id = '{sim_query_id}' ASYNC"
        ))
        .await
        .expect("issuing the kill");
    assert_eq!(killed.len(), 1, "the kill matched nothing: {killed:?}");
    if session.feed_sim(6, 1, 6, 0, 0).is_ok() {
        session
            .wait_sim(6, ROW_TIMEOUT)
            .await
            .expect_err("the killed statement cannot write the tic");
    }
    let recovery = session
        .recover(&conn)
        .await
        .expect("recover has to reopen both statements");
    run_tic(&session, recovery.resume_tic).await;

    session
        .close()
        .await
        .expect("the reopened statements run to the end");
    assert_eq!(
        settled(&admin, baseline).await,
        baseline,
        "the session left connections busy on the server"
    );

    conn_args("default")
        .connect()
        .run(&format!("DROP DATABASE IF EXISTS {database}"))
        .await
        .unwrap_or_else(|e| panic!("dropping {database}: {e}"));
}
