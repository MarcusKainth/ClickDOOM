//! The tic command built from key state, against a real ClickHouse server.
//!
//! A run of tics is fed key words and mouse deltas and the command each one
//! produces is checked against `native/tests/support/ticcmd.rs`, a reader
//! written from `g_game.c` rather than from the SQL.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickdoom_spec::native_state::key;
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::ticcmd::{Input, Ticcmd};

/// The tic the run starts at, past the melt so nothing else is moving.
const FIRST: u32 = 1;

/// What the session streams each tic: the key word and the two mouse
/// deltas.
///
/// The run holds a turn key past `SLOWTURNTICS` so the slow turn gives way
/// to the fast one, releases it so the count resets, then walks through
/// the strafe, the speed, the mouse and the weapon keys.
const RUN: [(u32, i16, i16); 16] = [
    (0, 0, 0),
    (key::UP, 0, 0),
    (key::UP | key::RIGHT, 0, 0),
    (key::UP | key::RIGHT, 0, 0),
    (key::UP | key::RIGHT, 0, 0),
    (key::UP | key::RIGHT, 0, 0),
    (key::UP | key::RIGHT, 0, 0),
    (key::UP | key::RIGHT | key::SPEED, 0, 0),
    (key::UP | key::LEFT | key::SPEED, 0, 0),
    (0, 0, 0),
    (key::LEFT | key::STRAFE, 0, 0),
    (key::DOWN | key::STRAFE_LEFT | key::SPEED, 0, 0),
    (key::STRAFE_RIGHT | key::FIRE | key::USE, 0, 0),
    (0, 40, -13),
    (key::STRAFE, 40, -13),
    (key::UP | key::SPEED | (1 << (key::WEAPON_SHIFT + 3)), 0, 90),
];

#[tokio::test]
async fn the_keys_build_the_command_the_engine_builds() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_input").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    let run: Vec<sim::tick::Input> = RUN
        .iter()
        .enumerate()
        .map(|(at, (keys, dx, dy))| sim::tick::Input::keys(FIRST + at as u32, *keys, (*dx, *dy)))
        .collect();
    plan.push(sim::tick::run_statement(&db, &run));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Built> = fixture
        .rows(&format!(
            "SELECT tic, p_cmd_forwardmove AS forwardmove, p_cmd_sidemove AS sidemove, \
             p_cmd_angleturn AS angleturn, p_cmd_buttons AS buttons, turnheld, \
             paused, leveltime \
             FROM {db}.native_state WHERE tic >= {FIRST} ORDER BY tic"
        ))
        .await;
    assert_eq!(rows.len(), RUN.len());

    let mut input = Input::default();
    for (row, (keys, dx, dy)) in rows.iter().zip(RUN) {
        let want = input.build(keys, (dx, dy));
        let got = Ticcmd {
            forwardmove: row.forwardmove,
            sidemove: row.sidemove,
            angleturn: row.angleturn,
            buttons: row.buttons,
        };
        assert_eq!(
            got, want,
            "the command at tic {} for keys {keys:#x}",
            row.tic
        );
        assert_eq!(row.turnheld, input.turnheld, "turnheld at tic {}", row.tic);
        assert_eq!(row.paused, 0, "nothing in the run pauses");
        assert_eq!(
            row.leveltime, row.tic as i32,
            "leveltime at tic {}",
            row.tic
        );
    }

    the_pause_key_stops_the_world(&fixture).await;
    fixture.finish().await;
}

#[derive(Row, Deserialize)]
struct Built {
    tic: u32,
    forwardmove: i8,
    sidemove: i8,
    angleturn: i16,
    buttons: u8,
    turnheld: i32,
    paused: u8,
    leveltime: i32,
}

/// `P_Ticker` returns before it runs anything while the game is paused, so
/// `leveltime` stops with it and starts again when the key comes back.
async fn the_pause_key_stops_the_world(fixture: &Fixture) {
    let db = &fixture.database;
    let last = FIRST + RUN.len() as u32 - 1;
    let leveltime = |tic: u32| {
        let sql = format!("SELECT leveltime FROM {db}.native_state WHERE tic = {tic}");
        async move { fixture.scalar::<i32>(&sql).await }
    };
    let running = leveltime(last).await;

    let presses: Vec<sim::tick::Input> = [key::PAUSE, 0, key::PAUSE, 0]
        .into_iter()
        .enumerate()
        .map(|(at, keys)| sim::tick::Input::keys(last + 1 + at as u32, keys, (0, 0)))
        .collect();
    fixture
        .execute(&[sim::tick::run_statement(db, &presses)])
        .await
        .unwrap();
    let paused: Vec<u8> = fixture
        .rows(&format!(
            "SELECT paused FROM {db}.native_state WHERE tic > {last} ORDER BY tic"
        ))
        .await;
    assert_eq!(paused, [1, 1, 0, 0], "the pause key toggles once a press");
    assert_eq!(
        leveltime(last + 1).await,
        running,
        "a paused tic holds the clock"
    );
    assert_eq!(leveltime(last + 2).await, running);
    assert_eq!(
        leveltime(last + 3).await,
        running + 1,
        "and it starts again"
    );
    assert_eq!(leveltime(last + 4).await, running + 2);
}
