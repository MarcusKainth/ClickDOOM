//! `P_ZMovement` for a thing that is not the player, against a real
//! ClickHouse server.
//!
//! The blood a shotgun pellet leaves is the only thing `demo3` drops
//! before the first divergence, and what it reaches is gravity pulling on
//! a thing already moving and on one that has just stopped.
//! `sim_parity_live` carries those tics against the engine's own trace.
//! The two ends of the routine the demo does not reach are seeded here:
//! the floor taking a thing that falls onto it, and the ceiling stopping
//! one that rises into it.
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
const BEFORE: u32 = 40;
const SEED_TIC: u32 = 41;

/// The slot the height is seeded onto. Slot 118 stands on its floor at
/// gametic 40 with a ceiling seventy-two units above it, so both ends of
/// the routine are within reach of where it already is and nothing has to
/// be moved sideways to get there.
const SLOT: usize = 118;

/// `p_local.h`
const GRAVITY: i64 = 1 << 16;

#[derive(Row, Deserialize)]
struct Fell {
    tic: u32,
    z: i32,
    momz: i32,
    floorz: i32,
    ceilingz: i32,
    height: i32,
    unresolved: u8,
}

async fn run(name: &str, z: i64, momz: i64) -> (Fell, Fell) {
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

    let put = |column: &'static str, value: String| {
        (
            column,
            format!(
                "arrayMap((v, k) -> toInt32(if(k = {SLOT}, {value}, v)), \
                 p.{column}, arrayEnumerate(p.{column}))"
            ),
        )
    };
    let overrides = [
        put("m_z", format!("p.m_floorz[{SLOT}] + {z}")),
        put("m_momz", momz.to_string()),
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
    let rows: Vec<Fell> = fixture
        .rows(&format!(
            "SELECT tic, m_z[{SLOT}] AS z, m_momz[{SLOT}] AS momz, \
             m_floorz[{SLOT}] AS floorz, m_ceilingz[{SLOT}] AS ceilingz, \
             m_height[{SLOT}] AS height, unresolved \
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
async fn a_thing_falling_onto_its_floor_lands_on_it() {
    // Sixteen units up and thirty-two units of fall, so the step goes
    // through the floor rather than onto it.
    let (before, after) = run("sim_fall_floor", 16 * 65536, -32 * 65536).await;
    assert_eq!(before.tic, SEED_TIC);
    assert_ne!(
        before.z, before.floorz,
        "the seeded row stands off its floor, or nothing would fall"
    );
    assert_eq!(after.unresolved, 0, "the tic runs");
    assert_eq!(after.z, after.floorz, "the floor takes it");
    assert_eq!(after.momz, 0, "and stops it");
}

#[tokio::test]
async fn a_thing_rising_into_its_ceiling_is_held_under_it() {
    // Straight up by more than the height of the room, so the step goes
    // through the ceiling. Gravity pulls on the way, because the thing is
    // above its floor for the whole of it.
    let (before, after) = run("sim_fall_ceiling", 0, 80 * 65536).await;
    assert_eq!(before.momz, 80 * 65536);
    assert!(
        before.ceilingz - before.floorz < before.momz,
        "the seeded momentum reaches past the ceiling, or nothing is clipped"
    );
    assert_eq!(after.unresolved, 0, "the tic runs");
    assert_eq!(
        after.z,
        after.ceilingz - after.height,
        "the ceiling holds it under itself"
    );
    assert_eq!(after.momz, 0, "and takes what was carrying it up");
}

#[tokio::test]
async fn a_thing_that_has_just_stopped_falling_takes_two_pulls() {
    // `P_ZMovement` gives a thing whose momentum has reached zero twice
    // the pull, which is what makes the first tic of a drop move further
    // than the tics after it.
    let (before, after) = run("sim_fall_start", 32 * 65536, 0).await;
    assert_eq!(before.momz, 0);
    assert_eq!(after.unresolved, 0, "the tic runs");
    assert_eq!(after.momz as i64, -2 * GRAVITY, "the first pull is doubled");
    assert_eq!(
        after.z, before.z,
        "and the height itself does not move until the tic after"
    );
}
