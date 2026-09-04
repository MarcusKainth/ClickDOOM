//! Live proof for the input path, run against a real ClickHouse server.
//!
//! `clickdoom native play` samples the keys and streams them as a tic's
//! input row. What this covers is the streaming: a key bit the driver sends
//! reaches the tic command the simulation builds, and the world moves the
//! way the command says, tic by tic, over the same session the command
//! opens.
//!
//! What a command does to the world in detail is the simulation's, and
//! `native/tests/sim_input_live.rs` is where that is checked. What is here
//! is that a key the driver sent is the key that arrived.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST`/`CLICKHOUSE_HTTP_PORT`/
//! `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123`).

#![cfg(feature = "clickhouse-tests")]

use std::time::Duration; // purity-ok: a timeout in the harness, never a value a statement reads

use clickdoom_driver::client::Db;
use clickdoom_driver::native::Session;
use clickdoom_native::sql;
use clickdoom_native::sql::sim::tick;
use clickdoom_spec::native_state::key;

mod support;

use support::{conn_args, drop_database, loaded};

/// `G_BuildTiccmd`'s `forwardmove`, walking and running.
const WALK: i8 = 0x19;
const RUN: i8 = 0x32;

/// The first tic of a session pays for the statement's analysis, which is
/// seconds. `TIC_TIMEOUT` is the budget for the tics after it.
const FIRST_ROW_TIMEOUT: Duration = Duration::from_secs(150);

/// The tic command the simulation built for `tic`, and where the player
/// stands after it.
async fn command(db: &Db, database: &str, tic: u32) -> (i8, i16, u8, i32) {
    db.fetch_one::<(i8, i16, u8, i32)>(&format!(
        "SELECT p_cmd_forwardmove, p_cmd_angleturn, paused, m_x[1] \
         FROM {database}.native_state WHERE tic = {tic}"
    ))
    .await
    .unwrap_or_else(|e| panic!("reading tic {tic}: {e}"))
}

#[tokio::test]
async fn a_key_the_driver_streams_reaches_the_tic_command() {
    let (database, admin) = loaded("keys").await;
    let conn = conn_args(&database);
    let session = Session::open(
        &conn,
        &database,
        Some(&tick::resident_statement(&database)),
        Some(&sql::render::frame_transform(&database)),
    )
    .await
    .expect("opening the session");
    assert!(session.has_sim());

    let start = command(&admin, &database, 0).await.3;

    // One tic per key state, streamed the way the paced loop streams them.
    let cases: [(u32, u32); 5] = [
        (1, key::UP),
        (2, key::UP | key::SPEED),
        (3, key::RIGHT),
        (4, key::PAUSE),
        (5, 0),
    ];
    for (tic, keys) in cases {
        session
            .feed_sim(tic, tick::source::KEYS, keys, 0, 0)
            .unwrap_or_else(|e| panic!("feeding tic {tic}: {e}"));
        session
            .wait_sim(tic, FIRST_ROW_TIMEOUT)
            .await
            .unwrap_or_else(|e| panic!("tic {tic}: {e}"));
    }
    session.close().await.expect("the statements finished");

    let (forward, _, _, _) = command(&admin, &database, 1).await;
    assert_eq!(forward, WALK, "a held forward key walks");
    let (forward, _, _, _) = command(&admin, &database, 2).await;
    assert_eq!(forward, RUN, "forward with the speed key runs");

    let (forward, turn, _, _) = command(&admin, &database, 3).await;
    assert_eq!(forward, 0, "nothing is carried from the tic before");
    assert!(turn < 0, "turning right takes the angle down, got {turn}");

    // The pause key is one press, not a hold: the bit is set for the tic it
    // goes down on, and the world stays paused after it.
    let (_, _, paused, _) = command(&admin, &database, 4).await;
    assert_eq!(paused, 1, "the pause key paused the game");
    let (_, _, paused, _) = command(&admin, &database, 5).await;
    assert_eq!(paused, 1, "releasing the key did not unpause it");

    // The command reaches the world: a walk forward moves the player, and a
    // tic with no keys leaves them where the tic before put them.
    let (_, _, _, after_walking) = command(&admin, &database, 2).await;
    assert_ne!(
        after_walking, start,
        "a held forward key did not move the player"
    );
    let (_, _, _, after_nothing) = command(&admin, &database, 5).await;
    let (_, _, _, before_nothing) = command(&admin, &database, 4).await;
    assert_eq!(
        after_nothing, before_nothing,
        "the player kept moving after the keys were released"
    );

    drop_database(&admin, &database).await;
}
