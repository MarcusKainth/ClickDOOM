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
//! Every arm is a row seeded into one session. A session pays the tic
//! statement's analysis once, and for a suite this size that is the whole
//! of what it costs.
//!
//! Needs a reachable ClickHouse (`CLICKHOUSE_HOST` / `CLICKHOUSE_HTTP_PORT`
//! / `CLICKHOUSE_PASSWORD`, defaulting to `localhost:8123` with no
//! password). Behind the `clickhouse-tests` feature, so a run without a
//! server visibly excludes them.
#![cfg(feature = "clickhouse-tests")]

use clickdoom_native::sql::sim;
use clickdoom_native::sql::sim::tick::Input;
use clickdoom_native::{load, sql, wad::Wad};
use clickhouse::Row;
use serde::Deserialize;

mod support;

use support::db::Fixture;
use support::seed;

/// The tic every arm copies its row from.
const BEFORE: u32 = 40;

/// The slot the height is seeded onto. Slot 118 stands on its floor at
/// gametic 40 with a ceiling seventy-two units above it, so both ends of
/// the routine are within reach of where it already is and nothing has to
/// be moved sideways to get there.
const SLOT: usize = 118;

/// `p_local.h`
const GRAVITY: i64 = 1 << 16;

/// One arm per seeded row: its name, where the copy of `BEFORE` lands, how
/// far above its floor the thing starts and what height it carries. The
/// tics are far apart so the arms cannot read each other's rows.
///
/// Sixteen units up and thirty-two units of fall goes through the floor
/// rather than onto it. Eighty units of rise goes through a ceiling
/// seventy-two units up. Thirty-two units up with nothing carrying it is
/// the tic a drop starts on.
const ARMS: [(&str, u32, i64, i64); 3] = [
    ("floor", 200, 16 * 65536, -32 * 65536),
    ("ceiling", 300, 0, 80 * 65536),
    ("start", 400, 32 * 65536, 0),
];

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

#[tokio::test]
async fn a_thing_falls_and_clips_the_way_the_engine_does() {
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let fixture = Fixture::create("sim_fall").await;
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
    let mut statements: Vec<sql::Statement> = Vec::new();
    for (_, at, above, momz) in ARMS {
        let overrides = [
            put("m_z", format!("p.m_floorz[{SLOT}] + {above}")),
            put("m_momz", momz.to_string()),
        ];
        statements.extend(
            seed::row(&db, at, BEFORE, &overrides)
                .into_iter()
                .map(sql::Statement::sql),
        );
        statements.push(sim::tick::run_statement(
            &db,
            &[Input::keys(at + 1, 0, (0, 0))],
        ));
    }
    if let Err(error) = fixture.execute(&statements).await {
        fixture.finish().await;
        panic!("{error}");
    }

    let wanted: Vec<String> = ARMS
        .iter()
        .flat_map(|(_, at, _, _)| [at.to_string(), (at + 1).to_string()])
        .collect();
    let rows: Vec<Fell> = fixture
        .rows(&format!(
            "SELECT tic, m_z[{SLOT}] AS z, m_momz[{SLOT}] AS momz, \
             m_floorz[{SLOT}] AS floorz, m_ceilingz[{SLOT}] AS ceilingz, \
             m_height[{SLOT}] AS height, unresolved \
             FROM {db}.native_state WHERE tic IN ({}) ORDER BY tic",
            wanted.join(", ")
        ))
        .await;
    fixture.finish().await;
    assert_eq!(
        rows.len(),
        ARMS.len() * 2,
        "a seeded row and a tic from it for every arm"
    );
    let at = |tic: u32| {
        rows.iter()
            .find(|row| row.tic == tic)
            .unwrap_or_else(|| panic!("no row for tic {tic}"))
    };

    // A thing falling onto its floor lands on it.
    let (before, after) = (at(200), at(201));
    assert_ne!(
        before.z, before.floorz,
        "the seeded row stands off its floor, or nothing would fall"
    );
    assert_eq!(after.unresolved, 0, "the floor arm runs");
    assert_eq!(after.z, after.floorz, "the floor takes it");
    assert_eq!(after.momz, 0, "and stops it");

    // A thing rising into its ceiling is held under it.
    let (before, after) = (at(300), at(301));
    assert!(
        before.ceilingz - before.floorz < before.momz,
        "the seeded momentum reaches past the ceiling, or nothing is clipped"
    );
    assert_eq!(after.unresolved, 0, "the ceiling arm runs");
    assert_eq!(
        after.z,
        after.ceilingz - after.height,
        "the ceiling holds it under itself"
    );
    assert_eq!(after.momz, 0, "and takes what was carrying it up");

    // `P_ZMovement` gives a thing whose momentum has reached zero twice the
    // pull, which is what makes the first tic of a drop move further than
    // the tics after it.
    let (before, after) = (at(400), at(401));
    assert_eq!(before.momz, 0);
    assert_eq!(after.unresolved, 0, "the starting arm runs");
    assert_eq!(after.momz as i64, -2 * GRAVITY, "the first pull is doubled");
    assert_eq!(
        after.z, before.z,
        "and the height itself does not move until the tic after"
    );
}
