//! The player's movement against a real ClickHouse server.
//!
//! The push, the bob and the view height are checked against
//! `native/tests/support/walk.rs`, a reader written from `p_user.c` and
//! `p_mobj.c`. What the blockmap decides is checked by carrying every tic
//! of the run through: `DEMO3`'s player crosses no wall until the tic the
//! slide takes over, and the run reaches past it.
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

/// How far the run goes. `DEMO3`'s player walks a corridor, meets a wall
/// at `CLEAR_TICS`, slides along it, and walks over the level's shotgun
/// on the last of these tics.
const RUN_TICS: u32 = 47;

/// The last tic of the walk down the corridor, where the wall first
/// changes where the player ends up.
const CLEAR_TICS: u32 = 32;

/// How far the reader beside this test follows. It works out a free move
/// and knows nothing of the blockmap, so it stops at the tic the wall
/// first changes where the player ends up. `sim_parity_live` carries that
/// tic against the engine itself.
const FREE_TICS: u32 = 31;

/// What `P_TouchSpecialThing` leaves after the clip the player walks onto.
const CLIP_AMMO: i32 = 60;
const BONUSADD: i32 = 6;

/// `doomdef.h`: the shotgun, and the value `readyweapon` keeps while
/// nothing is pending.
const WP_SHOTGUN: i32 = 2;
const WP_PISTOL: i32 = 1;
const WP_NOCHANGE: i32 = 10;

/// What `P_GiveWeapon` leaves after the shotgun: two clips of shells.
const SHOTGUN_SHELLS: i32 = 8;

#[tokio::test]
async fn the_player_walks_the_way_the_engine_walks() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_move").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, RUN_TICS));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let rows: Vec<Tic> = fixture
        .rows(&format!(
            "SELECT tic, m_x[p_mo] AS x, m_y[p_mo] AS y, m_angle[p_mo] AS angle, \
             m_momx[p_mo] AS momx, m_momy[p_mo] AS momy, m_state[p_mo] AS state, \
             p_bob, p_viewz, p_viewheight, p_ammo[1] AS clip, p_bonuscount, \
             p_ammo[2] AS shells, p_weaponowned[1 + {WP_SHOTGUN}] AS owns_shotgun, \
             p_pendingweapon, p_readyweapon, \
             p_message, hu_message, length(m_x) AS mobjs, unresolved, \
             p_cmd_forwardmove AS forwardmove, p_cmd_sidemove AS sidemove, \
             p_cmd_angleturn AS angleturn \
             FROM {db}.native_state ORDER BY tic"
        ))
        .await;
    assert_eq!(rows.len(), RUN_TICS as usize + 1);

    the_clear_tics_are_carried_through(&rows);
    the_push_and_the_bob_are_what_the_engine_works_out(&rows);
    the_clip_on_the_floor_is_picked_up_once(&rows);
    the_shotgun_is_taken_and_asked_for(&rows);

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
    shells: i32,
    owns_shotgun: i32,
    p_pendingweapon: i32,
    p_readyweapon: i32,
    p_message: u64,
    hu_message: u64,
    mobjs: u64,
    unresolved: u8,
    forwardmove: i8,
    sidemove: i8,
    angleturn: i16,
}

/// A tic the simulation could not produce in full says so, and none of
/// the walk down the first corridor is one of them.
fn the_clear_tics_are_carried_through(rows: &[Tic]) {
    for row in rows {
        assert_eq!(row.unresolved, 0, "tic {} was not carried through", row.tic);
    }
    assert_eq!(rows.last().expect("the run has a last tic").tic, RUN_TICS);
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
        .filter(|row| row.tic > 0 && row.tic <= FREE_TICS)
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

/// `P_GiveWeapon` takes the shotgun, gives the shells that come with it
/// and asks for it, and the ask reaches the state row.
///
/// The command asks for no weapon on any tic of this run, so a pending
/// weapon here can only be the pickup's.
fn the_shotgun_is_taken_and_asked_for(rows: &[Tic]) {
    let taken = rows
        .iter()
        .find(|row| row.owns_shotgun != 0)
        .expect("the player walks over the shotgun");
    assert_eq!(taken.tic, RUN_TICS, "the shotgun is taken on the last tic");
    assert_eq!(taken.shells, SHOTGUN_SHELLS);
    assert_eq!(taken.p_pendingweapon, WP_SHOTGUN, "the pickup asks for it");
    assert_eq!(
        taken.p_readyweapon, WP_PISTOL,
        "the weapon in hand is the psprites' to change"
    );
    assert_eq!(
        taken.hu_message,
        walk::message("You got the shotgun!"),
        "the widget is told what it was"
    );
    for row in rows.iter().filter(|row| row.tic < taken.tic) {
        assert_eq!(row.shells, 0, "no shells before the shotgun at {}", row.tic);
        assert_eq!(
            row.p_pendingweapon, WP_NOCHANGE,
            "nothing is pending at tic {}",
            row.tic
        );
    }
}

/// A move too fast for one step, blocked on its first half.
///
/// `P_XYMovement` halves the move before it tries it, and its loop runs
/// `while (xmove || ymove)`, so a finished move has spent both. That is
/// the engine's own condition and the first thing checked here.
///
/// The position is not the engine's. The demo never reaches the speed
/// that splits a move, so nothing arbitrates the geometry yet and the
/// numbers are what this simulation answers, recorded so a change to the
/// loop has to be deliberate.
///
/// The start and the momentum come from a sweep of demo3's own positions:
/// this is one of the places in `E1M7` where the wall the first half meets
/// leaves the second half somewhere to go.
#[tokio::test]
async fn a_fast_move_blocked_on_its_first_half_spends_the_second() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_move_split").await;
    let db = fixture.database.clone();
    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let mover = support::world::Mover {
        x: 23392724,
        y: 28237257,
        z: 0,
        momx: 16 << 16,
        momy: -(28 << 16),
        radius: 16 << 16,
        height: 56 << 16,
        angle: 0,
        uses: 0,
    };
    let rows: Vec<support::world::Moved> = fixture.rows(&support::world::select(&db, &mover)).await;
    fixture.finish().await;

    let moved = &rows[0];
    assert_eq!(
        (moved.xmove, moved.ymove, moved.phase),
        (0, 0, DONE),
        "the move has to be spent and the loop finished"
    );
    assert_eq!((moved.x, moved.y), (23508477, 25229679));
}

/// `mobj.rs`: the phase a finished move ends on.
const DONE: i64 = 5;
