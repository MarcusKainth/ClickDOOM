//! The player's movement against a real ClickHouse server.
//!
//! The push, the bob and the view height are checked against
//! `native/tests/support/walk.rs`, a reader written from `p_user.c` and
//! `p_mobj.c`. What the blockmap decides is checked by running until the
//! simulation says it cannot carry a tic through: the first thirty-one
//! tics of `DEMO3` cross no wall, and the thirty-second does.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, tables, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::walk;

/// How far the run goes. `DEMO3`'s player walks a corridor and touches
/// nothing solid until the tic after this one.
const CLEAR_TICS: u32 = 31;

/// What `P_TouchSpecialThing` leaves after the clip the player walks onto.
const CLIP_AMMO: i32 = 60;
const BONUSADD: i32 = 6;

#[tokio::test]
async fn the_player_walks_the_way_the_engine_walks() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_move").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, CLEAR_TICS + 1));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Tic> = fixture
        .rows(&format!(
            "SELECT tic, m_x[p_mo] AS x, m_y[p_mo] AS y, m_angle[p_mo] AS angle, \
             m_momx[p_mo] AS momx, m_momy[p_mo] AS momy, m_state[p_mo] AS state, \
             p_bob, p_viewz, p_viewheight, p_ammo[1] AS clip, p_bonuscount, \
             p_message, hu_message, length(m_x) AS mobjs, unresolved, \
             p_cmd_forwardmove AS forwardmove, p_cmd_sidemove AS sidemove, \
             p_cmd_angleturn AS angleturn \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await;
    assert_eq!(rows.len() as usize, CLEAR_TICS as usize + 2);

    the_clear_tics_are_carried_through(&rows);
    the_push_and_the_bob_are_what_the_engine_works_out(&rows);
    the_clip_on_the_floor_is_picked_up_once(&rows);

    fixture.finish().await;
}

#[derive(Row, Deserialize)]
struct Tic {
    tic: u32,
    x: i32,
    y: i32,
    angle: u32,
    momx: i32,
    momy: i32,
    state: i32,
    p_bob: i32,
    p_viewz: i32,
    p_viewheight: i32,
    clip: i32,
    p_bonuscount: i32,
    p_message: u64,
    hu_message: u64,
    mobjs: u64,
    unresolved: u8,
    forwardmove: i8,
    sidemove: i8,
    angleturn: i16,
}

/// A tic the simulation could not produce in full says so, and the walk
/// down the first corridor is not one of them.
fn the_clear_tics_are_carried_through(rows: &[Tic]) {
    for row in rows.iter().filter(|row| row.tic <= CLEAR_TICS) {
        assert_eq!(row.unresolved, 0, "tic {} was not carried through", row.tic);
    }
    let blocked = rows.last().expect("the run has a last tic");
    assert_eq!(blocked.tic, CLEAR_TICS + 1);
    assert_eq!(
        blocked.unresolved, 1,
        "the tic the player first meets a wall on is one the slide has to take over"
    );
}

/// `P_Thrust`, `P_XYMovement`'s friction and `P_CalcHeight`, against a
/// reader that follows them over the same commands.
fn the_push_and_the_bob_are_what_the_engine_works_out(rows: &[Tic]) {
    let finesine = tables::table("finesine").unwrap().ints("value").unwrap();
    let mut player = walk::Player {
        x: rows[0].x,
        y: rows[0].y,
        angle: rows[0].angle,
        momx: 0,
        momy: 0,
        viewheight: rows[0].p_viewheight,
        deltaviewheight: 0,
    };
    for row in rows
        .iter()
        .filter(|row| row.tic > 0 && row.tic <= CLEAR_TICS)
    {
        let step = player.tic(
            &finesine,
            row.forwardmove,
            row.sidemove,
            row.angleturn,
            (row.tic - 1) as i32,
        );
        assert_eq!(row.angle, player.angle, "angle at tic {}", row.tic);
        assert_eq!(row.p_bob, step.bob, "bob at tic {}", row.tic);
        assert_eq!(row.p_viewz, step.viewz, "viewz at tic {}", row.tic);
        assert_eq!(
            (row.x, row.y),
            (player.x, player.y),
            "position at tic {}",
            row.tic
        );
        assert_eq!(
            (row.momx, row.momy),
            (player.momx, player.momy),
            "momentum at tic {}",
            row.tic
        );
        assert!(
            row.state >= walk::S_PLAY_RUN1 && row.state < walk::S_PLAY_RUN1 + 4,
            "the player is in a walking frame at tic {}",
            row.tic
        );
    }
}

/// The clip the player walks onto is taken once, and the thing it was
/// leaves the list.
fn the_clip_on_the_floor_is_picked_up_once(rows: &[Tic]) {
    let before = &rows[0];
    let after = &rows[1];
    assert_eq!(after.mobjs + 1, before.mobjs, "the clip leaves the list");
    assert_eq!(after.clip, CLIP_AMMO, "one clip, not two");
    assert_eq!(after.p_bonuscount, BONUSADD);
    // `P_TouchSpecialThing` puts the message in the player's hand and
    // `HU_Ticker` takes it out again before the tic ends.
    assert_eq!(after.p_message, 0, "the player's hand is emptied");
    assert_eq!(
        after.hu_message,
        walk::message("Picked up a clip."),
        "the widget is told what it was"
    );
    for row in rows
        .iter()
        .filter(|row| row.tic >= 2 && row.tic <= CLEAR_TICS)
    {
        assert_eq!(
            row.clip, CLIP_AMMO,
            "nothing else is picked up at tic {}",
            row.tic
        );
        assert_eq!(row.mobjs, after.mobjs, "the list holds at tic {}", row.tic);
    }
}
