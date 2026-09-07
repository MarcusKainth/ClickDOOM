//! An in-flight missile exploding on a wall rather than a thing, against a
//! real ClickHouse server.
//!
//! Seeded from `demo3`'s own recorded run instead of hand-placed geometry:
//! `native/tests/fixtures/demo3-missile-wall.tsv` carries the reference
//! emulator's own row at gametic 223, the tic before a thrown fireball
//! explodes on a wall, so the geometry, the blockmap and every other thing
//! on the level's own list are exactly what the real engine had. Running
//! gametic 224 from it with the demo lump's own recorded command reproduces
//! the reference's own row at 224 field for field, including `prndindex`:
//! `missile::draws`'s own worst case reserves the explosion's own draw for
//! every missile the walk sets off, not only the ones a thing stops, so a
//! busy tic with everything else this does not seed still lands on the
//! same total the reference drew.
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

/// The reference emulator's own rows at gametic 223 and 224, `demo3`'s own
/// probe trace trimmed to the tic a thrown fireball explodes on a wall.
const PROBE_ROWS: &str = include_str!("fixtures/demo3-missile-wall.tsv");

const GAMETIC_BEFORE: u32 = 223;
const GAMETIC_HIT: u32 = 224;

/// `refemu/reference_traces/demo3/probe.9a6a47d01119.tsv`: the fireball's
/// own array position, both rows.
const MISSILE_SLOT: usize = 265;
/// `p_pspr.c`'s own `S_TBALL2`, `states.tsv` row 99: the frame
/// `P_ExplodeMissile` puts the missile in.
const TBALL2: i32 = 99;
/// `p_mobj.h`: `MF_SOLID | MF_SHOOTABLE | MF_MISSILE | MF_DROPOFF |
/// MF_NOGRAVITY`, the fireball's own flags in flight, and what
/// `P_ExplodeMissile` leaves once a wall stops it.
const EXPLODED_FLAGS: i32 = 1552;
/// The reference's own row at gametic 223: the fireball still in flight,
/// and its own `prndindex` there.
const BEFORE_X: i32 = 10181582;
const BEFORE_Y: i32 = -19973474;
const BEFORE_PRNDINDEX: u8 = 194;
/// The reference's own row at gametic 224, once the wall has stopped it.
const AFTER_PRNDINDEX: u8 = 197;

fn probe_line(gametic: u32) -> Vec<u8> {
    let line = PROBE_ROWS
        .lines()
        .find(|line| line.split('\t').nth(1) == Some(gametic.to_string().as_str()))
        .unwrap_or_else(|| panic!("no probe row for gametic {gametic}"));
    format!("{line}\n").into_bytes()
}

#[derive(Row, Deserialize)]
struct Exploded {
    missile_state: i32,
    missile_flags: i32,
    missile_momx: i32,
    missile_momy: i32,
    missile_x: i32,
    missile_y: i32,
    prndindex: u8,
    unresolved: u64,
}

/// A fireball already in flight, seeded from `demo3`'s own recorded run at
/// the tic before it explodes on a wall rather than a thing.
///
/// `refemu/reference_traces/demo3/probe.9a6a47d01119.tsv`'s own scan for a
/// missile that entered its own death frame with no mobj's own health
/// moving in the same tic names gametic 224 as the first: the fireball at
/// slot 265 goes from flying (state 97, momentum nonzero) to exploded
/// (state 99, `EXPLODED_FLAGS`, momentum zero) with nothing on the level's
/// own list taking damage, so a wall or a special line stopped it, not
/// `PIT_CheckThing`.
#[tokio::test]
async fn a_missile_that_hits_a_wall_reserves_its_own_explosion_draw() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_missile_wall").await;
    let db = fixture.database.clone();

    let mut plan = load::plan(&db, &wad);
    plan.extend(sql::level_statements(&db, support::MAP, support::DEMO));
    plan.extend(sim::load_statements(&db));
    if let Err(error) = fixture.execute(&plan).await {
        fixture.finish().await;
        panic!("{error}");
    }
    support::probe::load(&fixture, &probe_line(GAMETIC_BEFORE)).await;

    let before: u8 = fixture
        .scalar(&format!(
            "SELECT prndindex FROM {db}.native_state WHERE tic = {GAMETIC_BEFORE}"
        ))
        .await;
    assert_eq!(
        before, BEFORE_PRNDINDEX,
        "the seeded row carries the reference's own prndindex"
    );

    fixture
        .execute(&[sim::tick::demo_statement(&db, GAMETIC_HIT, GAMETIC_HIT)])
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    let after: Exploded = fixture
        .rows(&format!(
            "SELECT m_state[{MISSILE_SLOT}] AS missile_state, \
             m_flags[{MISSILE_SLOT}] AS missile_flags, \
             m_momx[{MISSILE_SLOT}] AS missile_momx, m_momy[{MISSILE_SLOT}] AS missile_momy, \
             m_x[{MISSILE_SLOT}] AS missile_x, m_y[{MISSILE_SLOT}] AS missile_y, \
             prndindex, unresolved \
             FROM {db}.native_state WHERE tic = {GAMETIC_HIT}"
        ))
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no row for tic {GAMETIC_HIT}"));
    fixture.finish().await;

    assert_eq!(after.unresolved, 0, "the wall stopping it resolves");
    assert_eq!(after.missile_state, TBALL2, "the wall's own explode frame");
    assert_eq!(after.missile_flags, EXPLODED_FLAGS);
    assert_eq!(after.missile_momx, 0, "the impact stops it");
    assert_eq!(after.missile_momy, 0);
    assert_eq!(
        after.missile_x, BEFORE_X,
        "a wall stops it dead, not part way"
    );
    assert_eq!(after.missile_y, BEFORE_Y);
    // The tic's own total draw count, not just this missile's own: a busy
    // tic this does not seed draws numbers for hundreds of other things,
    // and every one of them reads its own base off `mt_pure_draws`, which
    // carries this missile's own reserved explosion draw ahead of
    // whatever comes after it in slot order. Reserving nothing for a wall
    // the way the pre-fix count did would shift every one of those bases
    // by one, and the reference's own prndindex would not agree.
    assert_eq!(
        after.prndindex, AFTER_PRNDINDEX,
        "the whole tic's own draw count agrees with the reference, wall reservation included"
    );
}
