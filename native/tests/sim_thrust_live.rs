//! `P_XYMovement` for a thing that is not the player, against a real
//! ClickHouse server.
//!
//! `demo3` only ever gives a monster the momentum one shotgun pellet's
//! `P_DamageMobj` leaves, which is small enough to spend in one part, so
//! the move a monster makes there is checked in `sim_parity_live` against
//! the engine's own trace. The parts of the routine the demo does not
//! reach are seeded here: the halving a momentum over half of `MAXMOVE`
//! takes, the clamp above `MAXMOVE`, and a move a wall refuses.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

/// The tic the run stops on before the seeded row, and the row's own tic.
/// Gametic 40 is early enough that no monster has woken and the list still
/// holds the level's own things.
const BEFORE: u32 = 40;
const SEED_TIC: u32 = 41;

/// The slot the momentum is seeded onto. Slot 118 is the monster the
/// demo's player later shoots, and at gametic 40 it stands still.
const SLOT: usize = 118;

/// Where the thing is put before it moves, how wide it is made, and the
/// way it is sent.
///
/// A move of `MAXMOVE` covers thirty map units, which is further than the
/// monster can go from where it stands. This is where the demo's player
/// stands at gametic 26, part way down a corridor it walks the whole of,
/// so the way back up is open for as far as the move reaches, and the
/// player is fifty units past it by gametic 41. The thing is given a
/// radius of one unit as well, so what the corridor is shaped like cannot
/// decide what this reads; the width is no part of the arithmetic under
/// test.
const OPEN_X: i64 = 4_498_858;
const OPEN_Y: i64 = 28_739_495;
const NARROW: i64 = 65_536;

/// `p_mobj.c`
const MAXMOVE: i64 = 30 << 16;
const STOPSPEED: i64 = 0x1000;
const FRICTION: i64 = 0xe800;

/// A momentum over half of `MAXMOVE` on one axis, which is what makes the
/// engine spend the move in two parts, and an odd negative one on the
/// other, which the two halves round differently. The test on the split
/// only means anything while the axis carrying it is the one with room,
/// so the large one is the axis the corridor runs along.
const SPLIT_MOMY: i64 = MAXMOVE / 2 + 1;
const SPLIT_MOMX: i64 = -3;

/// A momentum over `MAXMOVE`, which the clamp cuts down before anything
/// else reads it.
const OVER_MOMY: i64 = MAXMOVE + 100_000;

#[derive(Row, Deserialize)]
struct Moved {
    tic: u32,
    x: i32,
    y: i32,
    momx: i32,
    momy: i32,
    unresolved: u8,
}

/// `FixedMul` against `FRICTION`, which is what `P_XYMovement` leaves on a
/// thing standing on its floor with speed to spare.
fn slowed(mom: i64) -> i32 {
    ((mom * FRICTION) >> 16) as i32
}

/// Where `P_XYMovement`'s loop puts a thing, given a momentum nothing
/// blocks. The first part takes the half C division leaves and the second
/// takes the half the shift leaves, so a negative odd axis loses a unit
/// against the axis that triggered the split.
fn spent(mom: i64) -> i64 {
    let mom = mom.clamp(-MAXMOVE, MAXMOVE);
    mom / 2 + (mom >> 1)
}

/// The momentum a thing keeps once the move is done.
///
/// `P_XYMovement` reads both axes together: a thing stops dead only when
/// neither reaches `STOPSPEED`, so an axis under it still takes friction
/// while the other axis is carrying speed.
fn left(momx: i64, momy: i64) -> (i32, i32) {
    let momx = momx.clamp(-MAXMOVE, MAXMOVE);
    let momy = momy.clamp(-MAXMOVE, MAXMOVE);
    let slow = |mom: i64| mom > -STOPSPEED && mom < STOPSPEED;
    if slow(momx) && slow(momy) {
        (0, 0)
    } else {
        (slowed(momx), slowed(momy))
    }
}

async fn run(name: &str, momx: i64, momy: i64) -> (Moved, Moved) {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create(name).await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    plan.push(sim::tick::demo_statement(&db, 1, BEFORE));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let put = |column: &'static str, value: i64| {
        (
            column,
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {SLOT}, {value}, v)), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    let overrides = [
        put("m_x", OPEN_X),
        put("m_y", OPEN_Y),
        put("m_radius", NARROW),
        put("m_momx", momx),
        put("m_momy", momy),
    ];
    let seeded: Vec<sql::Statement> = seed::row(&db, SEED_TIC, BEFORE, &overrides)
        .into_iter()
        .map(sql::Statement::sql)
        .collect();
    if let Err(error) = fixture.execute(&seeded).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let tic = sim::tick::demo_statement(&db, SEED_TIC + 1, SEED_TIC + 1);
    if let Err(error) = fixture.execute(&[tic]).await {
        fixture.finish().await;
        panic!("{error}");
    }
    let rows: Vec<Moved> = fixture
        .rows(&format!(
            "SELECT tic, m_x[{SLOT}] AS x, m_y[{SLOT}] AS y, \
             m_momx[{SLOT}] AS momx, m_momy[{SLOT}] AS momy, unresolved \
             FROM {db}.native_state WHERE tic IN ({SEED_TIC}, {}) ORDER BY tic",
            SEED_TIC + 1
        ))
        .await;
    fixture.finish().await;
    assert_eq!(rows.len(), 2, "the seeded row and the tic after it");
    let mut rows = rows.into_iter();
    (rows.next().unwrap(), rows.next().unwrap())
}

#[tokio::test]
async fn a_momentum_over_half_of_maxmove_is_spent_in_two_parts() {
    let (before, after) = run("sim_thrust_split", SPLIT_MOMX, SPLIT_MOMY).await;
    assert_eq!(before.tic, SEED_TIC);
    assert_eq!(
        (before.momx as i64, before.momy as i64),
        (SPLIT_MOMX, SPLIT_MOMY),
        "the seeded row carries the momentum the test asked for"
    );
    // The whole of the move has to land, or the wall and not the halving
    // is what this would be reading.
    assert_eq!(
        after.unresolved, 0,
        "the tic runs, so nothing the move met stopped it"
    );
    assert_eq!(
        (after.x as i64, after.y as i64),
        (
            before.x as i64 + spent(SPLIT_MOMX),
            before.y as i64 + spent(SPLIT_MOMY)
        ),
        "both parts of the move land"
    );
    assert_eq!(
        (after.momx, after.momy),
        left(SPLIT_MOMX, SPLIT_MOMY),
        "friction takes what the move left"
    );
    // The halving reaches both axes once either one triggers it, and the
    // two halves round in opposite directions, so an odd axis lands
    // differently depending on its sign. Reading the same value for both
    // would mean the split never happened.
    assert_ne!(
        spent(SPLIT_MOMY),
        SPLIT_MOMY,
        "the odd axis the split rides on loses a unit"
    );
    assert_eq!(
        spent(SPLIT_MOMX),
        SPLIT_MOMX,
        "the odd negative axis lands whole, because the first half \
         truncates towards zero where the second floors"
    );
}

#[tokio::test]
async fn a_momentum_over_maxmove_is_cut_down_to_it() {
    let (before, after) = run("sim_thrust_clamp", 0, OVER_MOMY).await;
    assert_eq!(
        before.momy as i64, OVER_MOMY,
        "the seeded row carries more than the clamp allows"
    );
    assert_eq!(
        after.unresolved, 0,
        "the tic runs, so nothing the move met stopped it"
    );
    assert_eq!(
        after.y as i64 - before.y as i64,
        spent(MAXMOVE),
        "the move is the clamp and not what was asked for"
    );
    assert_eq!(
        (after.momx, after.momy),
        left(0, OVER_MOMY),
        "friction reads the clamp too"
    );
}
